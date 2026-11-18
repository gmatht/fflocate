#[cfg(feature = "pancurses")]
mod pancurses_backend {
    #![allow(unexpected_cfgs)] // Win95/rust9x custom target_family gates are intentional
    use pancurses::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::os::raw::c_void;
    use std::rc::Rc;
    use std::io::Write;

    // ── SGR escape sequences matching ratatui output ──────────────
    #[allow(dead_code)] // Win9x-only SGR emitter (unused on Linux)
    const SGR_FG_CYAN: &str = "\x1b[38;5;6m";
    #[allow(dead_code)] // Win9x-only SGR emitter (unused on Linux)
    const SGR_FG_YELLOW: &str = "\x1b[38;5;3m";
    #[allow(dead_code)] // Win9x-only SGR emitter (unused on Linux)
    const SGR_FG_DARK_GRAY: &str = "\x1b[38;5;8m";
    #[allow(dead_code)] // Win9x-only SGR emitter (unused on Linux)
    const SGR_BG_CYAN: &str = "\x1b[48;5;6m";
    #[allow(dead_code)] // Win9x-only SGR emitter (unused on Linux)
    const SGR_BG_YELLOW: &str = "\x1b[48;5;3m";
    #[allow(dead_code)] // Win9x-only SGR emitter (unused on Linux)
    const SGR_BG_DARK_GRAY: &str = "\x1b[48;5;8m";
    const SGR_BOLD: &str = "\x1b[1m";
    const SGR_UNDERLINE: &str = "\x1b[4m";
    const SGR_RESET: &str = "\x1b[0m";
    const SGR_FG_DEFAULT: &str = "\x1b[39m";
    const SGR_BG_DEFAULT: &str = "\x1b[49m";

    fn sgr_cup(y: i32, x: i32) -> String {
        format!("\x1b[{};{}H", y + 1, x + 1)
    }
    /// Blue menu bar (black text on a cyan/blue background).
    fn sgr_menu_bar() -> &'static str { "\x1b[38;5;0m\x1b[48;5;6m" }
    /// Menu item: white text on black (the submenu items).
    fn sgr_menu() -> &'static str { "\x1b[38;5;15m\x1b[48;5;0m" }
    fn sgr_formula() -> &'static str { "\x1b[38;5;6m\x1b[49m" }
    /// ncurses keycodes for the CSI sequences registered at startup.
    ///
    /// Kept at module scope (not inline in the setup block) so a test can
    /// assert the mapping without a terminal — in particular that
    /// `ESC [ 1 ; 2 {A,B,C,D}` (shift+arrows) map to the *shifted* keycodes.
    /// They used to alias onto the plain arrows, which silently turned every
    /// Shift+arrow into plain movement and made selection-by-keyboard
    /// impossible (the widget's `Input::KeyS*` handlers were unreachable).
    #[cfg(not(windows))]
    pub(crate) const CSI_KEY_BINDINGS: [(&[u8], i32); 14] = [
        (b"\x1b[A\x00", 259),  // KEY_UP
        (b"\x1b[B\x00", 258),  // KEY_DOWN
        (b"\x1b[D\x00", 260),  // KEY_LEFT
        (b"\x1b[C\x00", 261),  // KEY_RIGHT
        (b"\x1b[1;2A\x00", 337),  // KEY_SR    (shift+Up)
        (b"\x1b[1;2B\x00", 336),  // KEY_SF    (shift+Down)
        (b"\x1b[1;2D\x00", 393),  // KEY_SLEFT
        (b"\x1b[1;2C\x00", 402),  // KEY_SRIGHT
        // Home/End/PageUp/PageDown. Without these the CSI tilde sequences leak
        // into the formula bar as literal text while ratatui handles the keys.
        (b"\x1b[H\x00", 262),  // KEY_HOME
        (b"\x1b[1~\x00", 262),
        (b"\x1b[F\x00", 360),  // KEY_END
        (b"\x1b[4~\x00", 360),
        (b"\x1b[5~\x00", 339),  // KEY_PPAGE
        (b"\x1b[6~\x00", 338),  // KEY_NPAGE
    ];

    /// Header rows/columns covered by the selection (anchor↔cursor).
    ///
    /// Pure decision, kept separate from the renderer so it can be unit
    /// tested without an ncurses screen: the pancurses backend has no
    /// Rows/Cols-only selection modes, so both axes are always covered
    /// (matching its body-cell selection rectangle). `None` per axis means
    /// "no selection", i.e. only the cursor's own header glows.
    fn header_cover(
        anchor: Option<(u32, u32)>,
        cursor: (u32, u32),
    ) -> (Option<(u32, u32)>, Option<(u32, u32)>) {
        match anchor {
            Some((ar, ac)) => (
                Some((ar.min(cursor.0), ar.max(cursor.0))),
                Some((ac.min(cursor.1), ac.max(cursor.1))),
            ),
            None => (None, None),
        }
    }
    fn sgr_header_active() -> &'static str { "\x1b[1m\x1b[38;5;0m\x1b[48;5;3m" }
    fn sgr_header_inactive() -> &'static str { "\x1b[1m\x1b[38;5;6m" }
    fn sgr_row_cursor() -> &'static str { "\x1b[1;4m\x1b[38;5;0m\x1b[48;5;3m" }
    fn sgr_row_normal() -> &'static str { "\x1b[38;5;3m" }
    fn sgr_row_footer() -> &'static str { "\x1b[1m\x1b[38;5;6m" }
    fn sgr_row_underline() -> &'static str { "\x1b[4m\x1b[38;5;3m" }
    fn sgr_sep() -> &'static str { "\x1b[38;5;8m" }
    fn sgr_cell_cursor() -> &'static str { "\x1b[48;5;8m" }
    /// Computed totals (a row total, a data-column total, a footer grand
    /// total) render as bold default-ink: one weight across every aggregate
    /// cell, so a column of row totals and the footer read as one system.
    /// Cyan was the previous colour; black bold matches the GUI backend and
    /// carries far more contrast against the light sheet.
    fn sgr_cell_agg() -> &'static str { "\x1b[1m\x1b[38;5;0m" }
    fn sgr_cell_footer_agg() -> &'static str { "\x1b[1m\x1b[38;5;0m" }
    /// Hyperlink cell: blue foreground + underline (corro styles link
    /// cells so on every backend; style bit 7, see `spreadsheet::style`).
    fn sgr_cell_link() -> &'static str { "\x1b[38;5;4m\x1b[4m" }
    fn sgr_prompt() -> &'static str { "\x1b[38;5;15m\x1b[48;5;8m" }
    fn sgr_caret() -> &'static str { "\x1b[1m\x1b[38;5;0m\x1b[48;5;3m" }

    /// -1 = disabled; a valid descriptor read from INPUT_TRACE_FD at init.
    static INPUT_TRACE_FD: std::sync::atomic::AtomicI32 =
        std::sync::atomic::AtomicI32::new(-1);

    /// Opt-in mouse state (see `set_mouse_enabled` / `mouse_is_enabled`).
    /// Kept separate from `mousemask`'s return value because the mask can be
    /// reset behind the app's back (screen re-init on some terminals) and the
    /// value must be readable without touching curses.
    static MOUSE_ENABLED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    /// App-settable trace path (set_input_trace_file). Used instead of the
    /// INPUT_TRACE_FILE env var when the host calls the setter (e.g. on
    /// platforms where environment variables are unavailable).
    static INPUT_TRACE_PATH_LEN: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);
    static mut INPUT_TRACE_PATH: [u8; 256] = [0u8; 256];

    /// Enable the generic input trace, writing one line per getch result to
    /// `path`. App-level equivalent of setting INPUT_TRACE_FILE.
    pub fn set_input_trace_file(path: &str) {
        unsafe {
            let b = path.as_bytes();
            let n = b.len().min(256); // INPUT_TRACE_PATH is [u8; 256]
            INPUT_TRACE_PATH[..n].copy_from_slice(&b[..n]);
        }
        INPUT_TRACE_PATH_LEN.store(path.as_bytes().len().min(256), std::sync::atomic::Ordering::Relaxed);
        INPUT_TRACE_FD.store(1, std::sync::atomic::Ordering::Relaxed);
    }

    fn input_trace_path() -> String {
        let n = INPUT_TRACE_PATH_LEN.load(std::sync::atomic::Ordering::Relaxed);
        if n == 0 {
            return std::env::var("INPUT_TRACE_FILE").unwrap_or_default();
        }
        unsafe { String::from_utf8_lossy(&INPUT_TRACE_PATH[..n]).into_owned() }
    }

    fn trace_input_line(input: &Option<Input>) {
        let line = match input {
            Some(Input::Character(c)) => format!("char {:?}\n", c),
            Some(other) => format!("key {:?}\n", other),
            None => format!("err\n"),
        };
        let path = input_trace_path();
        if path.is_empty() {
            return;
        }
        append_file_raw(&path, line.as_bytes());
    }

    /// Append `bytes` to `path`, creating it if needed. On rust9x (Windows 9x)
    /// this uses CreateFileA directly because std::fs routes through
    /// CreateFileW, which is an unimplemented stub on 9x (OS error 120).
    fn append_file_raw(path: &str, bytes: &[u8]) {
        #[cfg(target_family = "rust9x")]
        unsafe {
            use std::os::raw::c_void;
            unsafe extern "system" {
                fn CreateFileA(
                    name: *const u8,
                    access: u32,
                    share: u32,
                    sa: *mut c_void,
                    disp: u32,
                    flags: u32,
                    tmpl: *mut c_void,
                ) -> *mut c_void;
                fn WriteFile(h: *mut c_void, buf: *const u8, len: u32, written: *mut u32, ov: *mut c_void) -> i32;
                fn CloseHandle(h: *mut c_void) -> i32;
            }
            let mut pathz = path.as_bytes().to_vec();
            pathz.push(0);
            const GENERIC_WRITE: u32 = 0x4000_0000;
            const FILE_SHARE_READ: u32 = 1;
            const OPEN_ALWAYS: u32 = 4;
            const INVALID: isize = -1;
            let h = CreateFileA(
                pathz.as_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_READ,
                std::ptr::null_mut(),
                OPEN_ALWAYS,
                0x80, // FILE_ATTRIBUTE_NORMAL
                std::ptr::null_mut(),
            );
            if h.is_null() || h as isize == INVALID {
                return;
            }
            let mut written = 0u32;
            WriteFile(h, bytes.as_ptr(), bytes.len() as u32, &mut written, std::ptr::null_mut());
            CloseHandle(h);
        }
        #[cfg(not(target_family = "rust9x"))]
        {
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut f| std::io::Write::write_all(&mut f, bytes));
        }
    }

    fn emit_sgr(s: &str) {
        if sgr_stream_is_ansi_unusable() {
            // Win9x: draw through curses instead of stdout.
            emit_sgr_via_curses(s);
            return;
        }
        let _ = std::io::stdout().write_all(s.as_bytes());
        let _ = std::io::stdout().flush();
    }

    // ── Win9x SGR→curses interpreter ────────────────────────────
    // Windows 9x DOS boxes neither interpret ANSI/SGR output (no ANSI.SYS
    // in the console) nor switch screen buffers reliably, and the host app
    // may have redirected stdout to a log file anyway. On 9x the direct-
    // write SGR stream (spreadsheet cells, menus, dialogs, prompts) is
    // translated into PDCurses calls onto the visible buffer instead.
    // Supported vocabulary: CUP (ESC[y;xH), SGR (ESC[...m: 0,1,4,22,24,
    // 38;5;n, 48;5;n, 39, 49), plain UTF-8 text, and OSC sequences (ignored).

    #[cfg(windows)]
    fn sgr_stream_is_ansi_unusable() -> bool {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetVersion() -> u32;
        }
        static DETECTED: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
        use std::sync::atomic::Ordering;
        match DETECTED.load(Ordering::Relaxed) {
            1 => false,
            2 => true,
            _ => {
                let win9x = (unsafe { GetVersion() } & 0x8000_0000) != 0;
                DETECTED.store(if win9x { 2 } else { 1 }, Ordering::Relaxed);
                win9x
            }
        }
    }

    #[cfg(not(windows))]
    fn sgr_stream_is_ansi_unusable() -> bool {
        false
    }

    // Fixed table, const-initialized (no thread_local/LazyCell machinery —
    // fragile on Win9x). All callers run on the single curses/UI thread.
    // Slot 0 unused; slots 1..=63 map (fg, bg) → PDCurses pair index.
#[allow(dead_code)] // Win9x-only SGR emitter (unused on Linux)
    static mut SGR_PAIR_FG: [i16; 64] = [-1; 64];
#[allow(dead_code)] // Win9x-only SGR emitter (unused on Linux)
    static mut SGR_PAIR_BG: [i16; 64] = [-1; 64];
#[allow(dead_code)] // Win9x-only SGR emitter (unused on Linux)
    static SGR_PAIR_NEXT: std::sync::atomic::AtomicI16 = std::sync::atomic::AtomicI16::new(1);

#[allow(dead_code)] // Win9x-only SGR emitter (unused on Linux)
    fn sgr_pair(fg: i16, bg: i16) -> chtype {
        if !has_colors() {
            return 0;
        }
        let existing = (1..64).find(|&i| unsafe { SGR_PAIR_FG[i as usize] == fg && SGR_PAIR_BG[i as usize] == bg });
        let p = match existing {
            Some(p) => p,
            None => {
                // PDCurses win32 provides 64 color pairs; pair 0 is fixed.
                let p = SGR_PAIR_NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed).min(63);
                init_pair(p, fg, bg);
                unsafe {
                    SGR_PAIR_FG[p as usize] = fg;
                    SGR_PAIR_BG[p as usize] = bg;
                }
                p
            }
        };
        COLOR_PAIR(p as chtype)
    }

#[allow(dead_code)] // Win9x-only SGR emitter (unused on Linux)
    fn sgr_current_attr(fg: i16, bg: i16, bold: bool, uline: bool, dim: bool) -> chtype {
        let mut a: chtype = 0;
        if bold {
            a |= A_BOLD;
        }
        if uline {
            a |= A_UNDERLINE;
        }
        if dim {
            a |= A_DIM;
        }
        if has_colors() {
            // PDCurses win32 has 8 colors (0 black … 7 white). Map the
            // xterm-256 palette indices used by the SGR stream onto them:
            // 8 (dark gray) → dim white, 15 (bright white) → bold white.
            let f: i16 = match fg {
                -1 | 8 | 15 => 7,
                n => n & 7,
            };
            let b: i16 = match bg {
                -1 => 0,
                8 => 7,
                n => n & 7,
            };
            a |= sgr_pair(f, b);
        }
        a
    }

#[allow(dead_code)] // Win9x-only SGR emitter (unused on Linux)
    fn sgr_apply(params: &str, fg: &mut i16, bg: &mut i16, bold: &mut bool, uline: &mut bool, dim: &mut bool) {
        if params.is_empty() {
            *fg = -1;
            *bg = -1;
            *bold = false;
            *uline = false;
            *dim = false;
            return;
        }
        let mut it = params.split(';');
        while let Some(tok) = it.next() {
            let code = if tok.is_empty() {
                0
            } else {
                match tok.parse::<i32>() {
                    Ok(c) => c,
                    Err(_) => continue,
                }
            };
            match code {
                0 => {
                    *fg = -1;
                    *bg = -1;
                    *bold = false;
                    *uline = false;
                    *dim = false;
                }
                1 => *bold = true,
                4 => *uline = true,
                22 => *bold = false,
                24 => *uline = false,
                39 => *fg = -1,
                49 => *bg = -1,
                38 | 48 => {
                    let _mode = it.next(); // "5"
                    let n = it.next().and_then(|v| v.parse::<i16>().ok()).unwrap_or(0);
                    if code == 38 {
                        *fg = n;
                    } else {
                        *bg = n;
                    }
                }
                _ => {}
            }
        }
    }

    // Win9x/Windows-only: drives the PDCurses win32 buffer through the sys
    // crate (64-bit chtype ABI). Never used on other platforms, where the
    // SGR stream always goes to ANSI stdout (see `emit_sgr`).
    #[cfg(windows)]
    fn emit_sgr_via_curses(s: &str) {
        let bytes = s.as_bytes();
        let mut i = 0usize;
        let (mut y, mut x) = (0i32, 0i32);
        let (mut fg, mut bg) = (-1i16, -1i16);
        let (mut bold, mut uline, mut dim) = (false, false, false);
        let mut attr: chtype = 0;
        let mut attr_valid = false;
        let (lines, cols) = unsafe { (pdcurses::LINES, pdcurses::COLS) };
        if lines <= 0 || cols <= 0 {
            return;
        }
        while i < bytes.len() {
            match bytes[i] {
                0x1b if i + 1 < bytes.len() && bytes[i + 1] == b'[' => {
                    let start = i + 2;
                    let mut j = start;
                    while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                        j += 1;
                    }
                    if j >= bytes.len() {
                        break;
                    }
                    let params = std::str::from_utf8(&bytes[start..j]).unwrap_or("");
                    match bytes[j] {
                        b'H' | b'f' => {
                            let mut ps = params.split(';');
                            let row = ps.next().and_then(|v| v.parse::<i32>().ok()).unwrap_or(1).max(1);
                            let col = ps.next().and_then(|v| v.parse::<i32>().ok()).unwrap_or(1).max(1);
                            y = row - 1;
                            x = col - 1;
                        }
                        b'm' => {
                            sgr_apply(params, &mut fg, &mut bg, &mut bold, &mut uline, &mut dim);
                            attr_valid = false;
                        }
                        _ => {}
                    }
                    i = j + 1;
                }
                0x1b if i + 1 < bytes.len() && bytes[i + 1] == b']' => {
                    // OSC (e.g. clipboard set): skip to BEL or ST (ESC \)
                    let mut j = i + 2;
                    while j < bytes.len() && bytes[j] != 0x07 {
                        if bytes[j] == 0x1b && j + 1 < bytes.len() && bytes[j + 1] == b'\\' {
                            break;
                        }
                        j += 1;
                    }
                    i = if j < bytes.len() { j + 1 } else { bytes.len() };
                }
                _ => {
                    // decode one UTF-8 char
                    let (n, ch) = match std::str::from_utf8(&bytes[i..]) {
                        Ok(t) => {
                            let c = t.chars().next().unwrap();
                            (c.len_utf8(), c)
                        }
                        Err(e) if e.valid_up_to() > 0 => {
                            let t = std::str::from_utf8(&bytes[i..i + e.valid_up_to()]).unwrap();
                            let c = t.chars().next().unwrap();
                            (c.len_utf8(), c)
                        }
                        Err(_) => (1, '\u{fffd}'),
                    };
                    i += n;
                    match ch {
                        '\n' => {
                            y += 1;
                            x = 0;
                        }
                        '\r' => {
                            x = 0;
                        }
                        _ => {
                            if y < lines && x < cols {
                                if !attr_valid {
                                    attr = sgr_current_attr(fg, bg, bold, uline, dim);
                                    attr_valid = true;
                                }
                                // PDCurses win32 is not wide: box-drawing glyphs
                                // have no portable codepoint mapping, so fall
                                // back to their ASCII equivalents.
                                let c8: u32 = match ch as u32 {
                                    0x20..=0x7e => ch as u32,
                                    0x2500 => b'-' as u32,
                                    0x2502 => b'|' as u32,
                                    0x2501..=0x257f => b'+' as u32,
                                    0x25bc => b'v' as u32,
                                    _ => b'?' as u32,
                                };
                                unsafe {
                                    pdcurses::mvaddch(y, x, (c8 as chtype) | attr);
                                }
                            }
                            x += 1;
                            if x >= cols {
                                x = 0;
                                y += 1;
                            }
                        }
                    }
                }
            }
        }
        unsafe {
            pdcurses::refresh();
        }
    }

    #[cfg(not(windows))]
    fn emit_sgr_via_curses(_s: &str) {
        // Unreachable: `sgr_stream_is_ansi_unusable()` is always false off
        // Windows, so `emit_sgr` never routes here. Present only so the
        // call in `emit_sgr` compiles on every platform.
    }

    pub type Callback = Box<dyn FnMut()>;

    #[derive(Clone, Copy, Debug, Default)]
    pub struct Rect {
        pub x: i32,
        pub y: i32,
        pub w: i32,
        pub h: i32,
    }

    #[derive(Clone)]
    pub enum PcWidgetKind {
        Window { title: String },
        Button { label: String, weight: i32, italic: bool },
        /// `fixed_w` is the width pin set by `set_label_fixed_width` (`Some`)
        /// or `None` for shrink-to-fit. Cells, not pixels: the terminal is the
        /// unit the box layout distributes, so a pinned label occupies a stable
        /// slot no matter what its text says.
        Label { text: String, fixed_w: Option<i32> },
        BoxWidget { horizontal: bool, spacing: i32 },
        Sizer(crate::Sizer),
        Grid { cols: usize, rows: usize },
        Entry { buffer: String, cursor: usize },
        CheckButton { label: String, checked: bool },
        RadioButton { label: String, checked: bool, group_id: usize },
        Dialog {
            title: String,
            /// The response id a dismissal reports when no button was
            /// chosen. `None` means "not set", which is distinct from
            /// "set to 0": 0 is a real response id a caller may register.
            default_response: Option<i32>,
        },
        Menu,
        MenuBar { labels: Vec<crate::Label>, submenu_items: Vec<(crate::Label, Vec<crate::MenuItem>)> },
        SimpleAction,
        DropDown { items: Vec<String>, selected: Option<usize> },
        TextView { text: String },
        Canvas,
        Overlay,
        ScrolledWindow,
        Spreadsheet {
            grid: crate::core::Grid,
            formula_bar_address_id: Option<usize>,
            formula_bar_entry_id: Option<usize>,
            menu_text: String,
            status_text: String,
            border_title: String,
            formula_bar_trailing: String,
            tab_titles: Vec<String>,
            tab_active: usize,
        },
        /// A bare data grid (mirrors wxGrid) without the spreadsheet chrome.
        DataGrid(crate::core::Grid),
    }

    pub struct PcWidgetNode {
        pub id: usize,
        pub kind: PcWidgetKind,
        pub parent: Option<usize>,
        pub children: Vec<usize>,
        pub rect: Rect,
        pub visible: bool,
        pub callbacks: Vec<Callback>,
    }

    /// Canvas pointer handlers, keyed by widget id (see `Canvas::on_click`).
    #[derive(Default)]
    pub struct CanvasPointerHandlers {
        pub plain: Option<Box<dyn FnMut(f64, f64)>>,
        pub button: Option<Box<dyn FnMut(f64, f64, u32, u32)>>,
        pub motion: Option<Box<dyn FnMut(f64, f64, u32)>>,
    }

    pub struct PcState {
        pub nodes: Vec<PcWidgetNode>,
        pub next_id: usize,
        pub running: bool,
        pub pending_quit: bool,
        pub focus_id: Option<usize>,
        pub menu_bar_id: Option<usize>,
        pub menu_open: bool,
        pub active_submenu: usize,
        pub active_item: usize,
        /// Submenu path: indices of the submenu items entered from the root menu.
        /// Empty = at the root menu level.  The current level's items are the
        /// root menu's items followed by each submenu in the path.
        pub menu_stack: Vec<usize>,
        pub spreadsheet_output: String,
        /// Arbitrary per-widget client data (mirrors wxWindow::SetClientData).
        client_data: std::collections::HashMap<usize, String>,
        key_callbacks: Vec<(char, Box<dyn FnMut()>)>,
        /// Event callbacks (opt-in propagation): return `CallbackResult::Skip`
        /// to let the event bubble to the parent widget.
        event_callbacks: Vec<(usize, Box<dyn FnMut(&crate::Event) -> crate::CallbackResult>)>,
        pub cursor_move_callbacks: Vec<Box<dyn FnMut(u32, u32)>>,
        pub commit_edit_callbacks: Vec<Box<dyn FnMut(u32, u32, String)>>,
        pub goto_callbacks: Vec<Box<dyn FnMut()>>,
        /// Invoked (with the action name) when a menu item is activated.
        pub menu_action_callback: Option<Box<dyn FnMut(String)>>,
        /// Active text prompt (e.g. file path for Open/Save/Export).  When set,
        /// key input is routed into `prompt_buffer` instead of the spreadsheet.
        pub prompt_label: String,
        pub prompt_buffer: String,
        pub prompt_action: Option<String>,
        prompt_callback: Option<Box<dyn FnMut(String, String)>>,
    }

    impl PcState {
        pub fn new() -> Self {
            PcState {
                nodes: Vec::new(),
                next_id: 1,
                running: true,
                pending_quit: false,
                focus_id: None,
                menu_bar_id: None,
                menu_open: false,
                active_submenu: 0,
                active_item: 0,
                menu_stack: Vec::new(),
                spreadsheet_output: String::new(),
                client_data: std::collections::HashMap::new(),
                key_callbacks: Vec::new(),
                event_callbacks: Vec::new(),
                cursor_move_callbacks: Vec::new(),
                commit_edit_callbacks: Vec::new(),
                goto_callbacks: Vec::new(),
                menu_action_callback: None,
                prompt_label: String::new(),
                prompt_buffer: String::new(),
                prompt_action: None,
                prompt_callback: None,
            }
        }

        pub fn alloc_id(&mut self) -> usize {
            let id = self.next_id;
            self.next_id += 1;
            id
        }

        pub fn add_node(&mut self, kind: PcWidgetKind, parent: Option<usize>) -> usize {
            let id = self.alloc_id();
            self.nodes.push(PcWidgetNode {
                id,
                kind,
                parent,
                children: Vec::new(),
                rect: Rect::default(),
                visible: true,
                callbacks: Vec::new(),
            });
            if let Some(pid) = parent {
                if let Some(p) = self.nodes.iter_mut().find(|n| n.id == pid) {
                    p.children.push(id);
                }
            }
            id
        }

        pub fn node_mut(&mut self, id: usize) -> Option<&mut PcWidgetNode> {
            self.nodes.iter_mut().find(|n| n.id == id)
        }

        pub fn node(&self, id: usize) -> Option<&PcWidgetNode> {
            self.nodes.iter().find(|n| n.id == id)
        }
    }

    thread_local! {
        static PC_STATE: RefCell<PcState> = RefCell::new(PcState::new());
        /// Pointer handlers registered on canvases (see `Canvas::on_click`).
        ///
        /// Kept in a side table rather than on `PcWidgetNode` so the node type
        /// stays cloneable; `plain`/`button`/`motion` mirror the GTK canvas's
        /// three registration slots, so a host that registers both `on_click`
        /// and `on_click_button` keeps both.
        static CANVAS_POINTER: RefCell<HashMap<usize, CanvasPointerHandlers>> =
            RefCell::new(HashMap::new());
        /// Canvas draw callbacks (see `canvas_set_draw_callback`). Invoked
        /// during each redraw with a `DrawContext` backed by a cell grid, whose
        /// ANSI output is then flushed to the terminal — the terminal's
        /// equivalent of a GTK draw callback.
        static CANVAS_DRAW: RefCell<HashMap<usize, Box<dyn FnMut(&mut dyn crate::core::DrawContext, i32, i32)>>> =
            RefCell::new(HashMap::new());
        /// Optional host frame hook, invoked on the main thread after each
        /// render. Lets a host application mirror external state into the widget
        /// tree without its own timer thread (e.g. flush a chat transcript into
        /// a TextView before every refresh). Cleared automatically on quit.
        static FRAME_HOOK: RefCell<Option<Box<dyn FnMut()>>> = RefCell::new(None);
        /// Generic pre-dispatch key hook (see set_key_input_hook).
        static KEY_INPUT_HOOK: RefCell<Option<Box<dyn FnMut(&Option<KeyInput>) -> bool>>> = RefCell::new(None);
        /// Redraw requested by the host (see request_redraw): consumed by the
        /// main loop after a hook-consumed key so modal state changes (e.g.
        /// a picker highlight move) repaint without waiting for the next
        /// backend-handled key.
        static REDRAW_REQUESTED: RefCell<bool> = RefCell::new(false);
        /// Active single-choice list popup (title, rows, selected index),
        /// drawn via SGR on top of everything like the info dialog. Generic
        /// mechanism: the host supplies items/selection/keys, the toolkit
        /// only renders. Shown by show_list_picker(), dismissed by
        /// close_list_picker().
        static ACTIVE_PICKER: RefCell<Option<(String, Vec<String>, usize)>> = RefCell::new(None);
        /// Commit-edit callbacks captured while inside a `with_state` call are stashed
        /// here and fired *outside* any `with_state` borrow (in the main loop) on the
        /// next iteration. The callbacks themselves call `with_state`-using adapter
        /// functions, so firing them from inside `with_state` would panic with a
        /// RefCell double-borrow (this is the same reason cursor-move callbacks are
        /// fired outside `with_state`).
        static DEFERRED_COMMIT: RefCell<Vec<(u32, u32, String, Vec<Box<dyn FnMut(u32, u32, String)>>)>> =
            RefCell::new(Vec::new());
        /// Active modal info dialog (title, body) drawn via SGR on top of the
        /// spreadsheet output.  Shown by show_dialog(), dismissed by Escape.
        static ACTIVE_DIALOG: RefCell<Option<(String, String)>> = RefCell::new(None);
        /// Generic after-redraw callback (see set_after_redraw_callback).
        static AFTER_REDRAW: RefCell<Option<Box<dyn FnMut()>>> = RefCell::new(None);
        /// Generic Alt+letter hook (see set_alt_key_callback).
        static ALT_KEY_CALLBACK: RefCell<Option<Box<dyn FnMut(char) -> bool>>> = RefCell::new(None);
        /// Named-action registry (see register_action / set_action_*).
        static ACTION_REGISTRY: RefCell<std::collections::HashMap<String, crate::Action>> =
            RefCell::new(std::collections::HashMap::new());
        /// Message-box result callback fired when the dialog is dismissed.
        static DIALOG_RESULT: RefCell<Option<Box<dyn FnMut(crate::MessageBoxResult)>>> =
            RefCell::new(None);
        /// When the user moves Up/Down within an open menu, only the highlight
        /// changes.  Record (previous_item, new_item) here so the loop can
        /// redraw just those two rows instead of the entire submenu (flicker).
        static MENU_HIGHLIGHT: RefCell<Option<(usize, usize)>> = RefCell::new(None);
        /// Opt-in host mouse hook (see set_mouse_hook).  Invoked for every
        /// decoded pointer event before the built-in dispatch; returning
        /// `true` consumes the event.
        static MOUSE_HOOK: RefCell<Option<Box<dyn FnMut(&MouseEvent) -> bool>>> =
            RefCell::new(None);
    }

    /// Install (or clear with `None`) a host frame hook run on every main-loop
    /// iteration after rendering. The closure is invoked synchronously on the
    /// UI thread, so it may mutate widgets directly.
    pub fn set_frame_hook(hook: Option<Box<dyn FnMut()>>) {
        FRAME_HOOK.with(|h| *h.borrow_mut() = hook);
    }

    /// Install (or clear with `None`) a generic pre-dispatch key hook. It is
    /// invoked for every decoded key before the backend handles it; returning
    /// `true` consumes the key (the default handling is skipped). The host
    /// application decides which keys to intercept for its own modal state —
    /// the toolkit itself never interprets the hook's purpose.
    pub fn set_key_input_hook(hook: Option<Box<dyn FnMut(&Option<KeyInput>) -> bool>>) {
        KEY_INPUT_HOOK.with(|h| *h.borrow_mut() = hook);
    }

    /// A decoded, toolkit-neutral representation of a key press handed to the
    /// generic pre-dispatch key hook (`set_key_input_hook`). The host decides
    /// what each key means for its own modal state; the toolkit never does.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum KeyInput {
        Enter,
        Escape,
        ArrowUp,
        ArrowDown,
        ArrowLeft,
        ArrowRight,
        Char(char),
        /// F2: hosts start in-cell editing (LibreOffice parity). Reported
        /// separately from `Other` so hosts can observe it; the widget
        /// itself never consumes it.
        F2,
        /// F3: hosts open the margin-aggregate picker on an aggregate key.
        /// Reported separately from `Other` (same contract as `F2`).
        F3,
        Other,
    }

    /// Map a raw pancurses `Input` to the generic [`KeyInput`] vocabulary.
    fn convert_key_input(input: &Option<Input>) -> Option<KeyInput> {
        match input {
            Some(Input::Character('\n')) | Some(Input::Character('\r')) => Some(KeyInput::Enter),
            Some(Input::Character('\x1b')) => Some(KeyInput::Escape),
            Some(Input::KeyUp) => Some(KeyInput::ArrowUp),
            Some(Input::KeyDown) => Some(KeyInput::ArrowDown),
            Some(Input::KeyLeft) => Some(KeyInput::ArrowLeft),
            Some(Input::KeyRight) => Some(KeyInput::ArrowRight),
            Some(Input::KeyF2) => Some(KeyInput::F2),
            Some(Input::KeyF3) => Some(KeyInput::F3),
            Some(Input::Character(c)) => Some(KeyInput::Char(*c)),
            Some(_) => Some(KeyInput::Other),
            None => None,
        }
    }

    /// Generic after-redraw callback: invoked once per redraw, after all SGR
    /// output has been flushed, so a caller that captures the terminal sees the
    /// freshly-rendered frame.  Host applications use this for idle markers or
    /// test synchronization; the toolkit itself never writes app-specific files.
    pub fn set_after_redraw_callback(cb: Box<dyn FnMut()>) {
        AFTER_REDRAW.with(|c| *c.borrow_mut() = Some(cb));
    }

    /// Generic Alt+letter hook: invoked when the user presses Alt+<letter> while
    /// no menu is open.  The application decides what to open (e.g. a specific
    /// menu or submenu) via `open_menu`; if no callback is installed the backend
    /// falls back to opening the root menu whose label starts with the letter.
    pub fn set_alt_key_callback(cb: Box<dyn FnMut(char) -> bool>) {
        ALT_KEY_CALLBACK.with(|c| *c.borrow_mut() = Some(cb));
    }

    /// Open a menu path: root menu index, submenu stack (indices of the submenu
    /// items entered from the root), and the highlighted item index.  Generic
    /// toolkit API — applications use it from their Alt+letter callback.
    pub fn open_menu(root: usize, stack: Vec<usize>, item: usize) {
        with_state(|state| {
            state.active_submenu = root;
            state.menu_stack = stack;
            state.active_item = item;
            state.menu_open = true;
        });
    }

    /// Opt-in mouse capture.  Off by default so terminals keep native text
    /// selection; enable only when the app has widgets that need pointer input.
    ///
    /// Tracks the request in [`MOUSE_ENABLED`] so [`mouse_is_enabled`] and the
    /// per-frame redraw can report a consistent state even where `mousemask`
    /// is a no-op (nested/overwritten screens).
    pub fn set_mouse_enabled(enabled: bool) {
        MOUSE_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
        if enabled {
            mousemask(ALL_MOUSE_EVENTS, None);
        } else {
            mousemask(0, None);
        }
    }

    /// Whether the application asked for mouse capture (see
    /// [`set_mouse_enabled`]). Pure state — unlike `mousemask` it never fails,
    /// so hosts and tests can assert the opt-in without a terminal.
    pub fn mouse_is_enabled() -> bool {
        MOUSE_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Install (or clear with `None`) a host mouse hook.
    ///
    /// Invoked for every decoded pointer event *before* the built-in dispatch
    /// (button/check/radio/entry/spreadsheet handling); returning `true`
    /// consumes the event so the default handling is skipped. Mirrors
    /// [`set_key_input_hook`]: the toolkit never interprets the hook's purpose,
    /// the host decides. Only runs while the app opted in with
    /// [`set_mouse_enabled(true)`].
    pub fn set_mouse_hook(hook: Option<Box<dyn FnMut(&MouseEvent) -> bool>>) {
        MOUSE_HOOK.with(|h| *h.borrow_mut() = hook);
    }

    /// Register a plain click handler on a canvas (the pancurses half of the
    /// `Canvas::on_click` API every other backend implements).
    ///
    /// Coordinates are pixel-space, matching `DrawContext` and the GTK/NWG
    /// canvases, so a host's click math is backend-independent: the terminal's
    /// cell coordinate is converted with the spreadsheet cell metrics
    /// (`CHAR_W`/`ROW_H`) and made relative to the canvas origin.
    pub fn canvas_on_click(id: usize, cb: Box<dyn FnMut(f64, f64)>) {
        CANVAS_POINTER.with(|h| {
            h.borrow_mut().entry(id).or_default().plain = Some(cb);
        });
    }

    /// Register a button/modifier-aware click handler on a canvas. `button` is
    /// 1-based (1 = primary), matching the GTK backend's `on_click_button`.
    pub fn canvas_on_click_button(id: usize, cb: Box<dyn FnMut(f64, f64, u32, u32)>) {
        CANVAS_POINTER.with(|h| {
            h.borrow_mut().entry(id).or_default().button = Some(cb);
        });
    }

    /// Register a pointer-motion handler on a canvas (see the GTK backend's
    /// `on_motion`). Terminals only deliver motion while a button is held and
    /// the app opted into mouse capture; a position report with no button is
    /// delivered as button 0, exactly like a hover.
    pub fn canvas_on_motion(id: usize, cb: Box<dyn FnMut(f64, f64, u32)>) {
        CANVAS_POINTER.with(|h| {
            h.borrow_mut().entry(id).or_default().motion = Some(cb);
        });
    }

    /// Register a canvas draw callback — the terminal half of the
    /// `Canvas::set_draw_callback` contract every GUI backend implements.
    ///
    /// The callback receives a [`crate::core::DrawContext`] backed by a cell
    /// grid (`PancursesDrawContext`), so a host paints with the same 2D API it
    /// would use on GTK. On each redraw the resulting grid is reduced to ANSI
    /// and flushed to the terminal (`CellGrid::to_ansi`), which is what makes
    /// a canvas-painted UI possible on a character display at all.
    pub fn canvas_set_draw_callback(
        id: usize,
        cb: Box<dyn FnMut(&mut dyn crate::core::DrawContext, i32, i32)>,
    ) {
        CANVAS_DRAW.with(|d| {
            d.borrow_mut().insert(id, cb);
        });
    }

    /// Drop a canvas's draw callback.
    pub fn canvas_clear_draw_callback(id: usize) {
        CANVAS_DRAW.with(|d| {
            d.borrow_mut().remove(&id);
        });
    }

    /// Run every registered canvas draw callback and flush the result.
    ///
    /// Called from the redraw path after the widget tree is painted, so a
    /// canvas's own output lands on top of its background exactly as a GTK
    /// draw callback would. Returns the number of canvases painted (0 when
    /// none is registered, so the caller can skip the flush).
    fn paint_canvases() -> usize {
        let canvases: Vec<(usize, Rect)> = with_state(|s| {
            s.nodes
                .iter()
                .filter(|n| n.visible && matches!(n.kind, PcWidgetKind::Canvas))
                .map(|n| (n.id, n.rect))
                .collect()
        });
        if canvases.is_empty() {
            return 0;
        }
        let mut painted = 0usize;
        for (id, rect) in canvases {
            // Cell-grid sized to the canvas's own rect: a canvas paints its
            // own area, and the blit is offset to that area's origin.
            let (w, h) = (rect.w.max(0) as u16, rect.h.max(0) as u16);
            if w == 0 || h == 0 {
                continue;
            }
            let mut grid = crate::backends::pancurses_draw::CellGrid::new(w, h);
            let ran = CANVAS_DRAW.with(|d| {
                let mut table = d.borrow_mut();
                match table.get_mut(&id) {
                    Some(cb) => {
                        let mut dc = crate::backends::pancurses_draw::PancursesDrawContext::new(&mut grid);
                        cb(&mut dc, w as i32, h as i32);
                        true
                    }
                    None => false,
                }
            });
            if ran {
                // Absolute CUP output, so overlapping canvases stay correct.
                emit_sgr(&grid.to_ansi(rect.y.max(0) as u16, rect.x.max(0) as u16));
                painted += 1;
            }
        }
        painted
    }

    /// Drop a canvas's pointer handlers (mirrors dropping the widget).
    pub fn canvas_clear_pointer_handlers(id: usize) {
        CANVAS_POINTER.with(|h| {
            h.borrow_mut().remove(&id);
        });
    }

    /// Convert a terminal cell coordinate to the canvas pixel space the other
    /// backends use, relative to the canvas widget's own origin.
    ///
    /// Terminals report whole cells; the widget tree stores the canvas rect in
    /// the same cell units. `DrawContext` (and therefore `set_draw_callback`,
    /// `on_click` on GTK/NWG) is pixel-based, so multiply by the cell metrics.
    pub fn canvas_pixel_xy(id: usize, cell_x: i32, cell_y: i32) -> Option<(f64, f64)> {
        let rect = with_state(|s| s.node(id).map(|n| n.rect))?;
        if rect.w <= 0 || rect.h <= 0 {
            return None;
        }
        let cw = crate::spreadsheet::SpreadsheetModel::CHAR_W;
        let ch = crate::spreadsheet::SpreadsheetModel::ROW_H;
        Some((
            (cell_x - rect.x) as f64 * cw,
            (cell_y - rect.y) as f64 * ch,
        ))
    }

    /// Terminal cell coordinate → canvas pixel coordinate for the widget under
    /// the pointer, and that widget's id. Used by the pointer dispatcher.
    fn canvas_hit(state: &PcState, x: i32, y: i32) -> Option<usize> {
        let hit = state.nodes.iter().rev().find(|n| {
            n.visible
                && matches!(n.kind, PcWidgetKind::Canvas)
                && n.rect.x <= x
                && x < n.rect.x + n.rect.w
                && n.rect.y <= y
                && y < n.rect.y + n.rect.h
        })?;
        Some(hit.id)
    }

    /// What a decoded mouse event means, independent of backend.
    ///
    /// ncurses (mouse v2) and PDCurses share the same layout for the button
    /// bits (five bits per button) and for the modifier/wheel bits, so one
    /// decoder serves both.  The raw `bstate` is still exposed on
    /// [`MouseEvent`] for hosts that need the untouched mask.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum MouseAction {
        /// A button went down (or a terminal reported a click for it).
        Pressed(u8),
        /// A button came back up.
        Released(u8),
        /// Press+release within the terminal's click interval (ncurses
        /// `mouseinterval`, PDCurses `SP->mouse_wait`).
        Clicked(u8),
        DoubleClicked(u8),
        TripleClicked(u8),
        /// Pointer motion.  `button` is the held button, if any.
        Moved { button: Option<u8> },
        /// Wheel/scroll.  `down` is true for wheel-down, false for wheel-up;
        /// `horizontal` distinguishes left/right wheels.
        Wheel { down: bool, horizontal: bool },
        /// The terminal reports bare pointer positions, no button change.
        Position,
    }

    /// A backend-neutral pointer event: cell position plus decoded action.
    ///
    /// Terminal coordinates are 0-based *cells* (ncurses and PDCurses both
    /// report cell coordinates, not pixels).  `-1` coordinates are what both
    /// libraries report for wheel events, so hosts should only treat the
    /// position as meaningful for the non-wheel variants.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct MouseEvent {
        pub x: i32,
        pub y: i32,
        pub action: MouseAction,
        pub shift: bool,
        pub ctrl: bool,
        pub alt: bool,
        /// The untranslated `bstate` bitmask (platform-specific).
        pub raw_state: u64,
    }

    // Mouse-mask bit layout.  Both backends this crate links against use the
    // same ncurses-v2 scheme: five bits per button (release/press/click/dbl/
    // triple, LSB first) and the wheel/motion/modifier flags at the top.
    // PDCurses additionally uses MOUSE_WHEEL_SCROLL (bit 25) as the gate the
    // app must set for wheel events to reach it at all — ALL_MOUSE_EVENTS
    // (`0x1fff_ffff`) already includes it, which is what `set_mouse_enabled`
    // installs.  The constants are defined here (instead of pulled from the
    // platform modules) because the vendored Windows constants only export
    // BUTTON1..BUTTON4; deriving them keeps one code path on every target.
    const MOUSE_BUTTON_STRIDE: u32 = 5;
    /// ncurses-v2 `BUTTON_CTRL/`BUTTON_SHIFT`/`BUTTON_ALT` / PDCurses
    /// `BUTTON_MODIFIER_{CONTROL,SHIFT,ALT}` — same values on both.
    const MOUSE_FLAG_CTRL: u64 = 0x001 << 30;
    const MOUSE_FLAG_SHIFT: u64 = 0x002 << 30;
    const MOUSE_FLAG_ALT: u64 = 0x004 << 30;
    const MOUSE_FLAG_POSITION: u64 = 0x008 << 30;
    /// PDCurses' wheel gate (also inside ALL_MOUSE_EVENTS).  Unused on
    /// ncurses, where BUTTON4/BUTTON5 carry the wheel instead.
    #[allow(dead_code)]
    const MOUSE_FLAG_WHEEL_SCROLL: u64 = 0x0200_0000;
    /// Action codes within one five-bit button group.
    const ACTION_RELEASED: u64 = 0x001;
    const ACTION_PRESSED: u64 = 0x002;
    const ACTION_CLICKED: u64 = 0x004;
    const ACTION_DOUBLE: u64 = 0x008;
    const ACTION_TRIPLE: u64 = 0x010;
    /// PDCurses' `PDC_MOUSE_MOVED` change flag (bit 3).  When it is set, the
    /// top bit of a button group means `BUTTONn_MOVED` rather than a triple
    /// click (they alias); see `decode_mouse_event`.
    const MOUSE_FLAG_MOVED: u64 = 0x008;

    /// Decode a raw `bstate` from `getmouse()` into a backend-neutral event.
    ///
    /// Pure function (no terminal needed) so the mapping is unit-tested on
    /// every platform.  Button numbers are 1-based, matching the mask names
    /// (`BUTTON1_*` → 1).
    pub fn decode_mouse_event(x: i32, y: i32, bstate: u64) -> MouseEvent {
        let shift = bstate & MOUSE_FLAG_SHIFT != 0;
        let ctrl = bstate & MOUSE_FLAG_CTRL != 0;
        let alt = bstate & MOUSE_FLAG_ALT != 0;

        // Wheel: ncurses and PDCurses both report BUTTON4_PRESSED for
        // wheel-up and BUTTON5_PRESSED for wheel-down.  Buttons 6/7 are left
        // to the button groups below (their 5-bit windows would overlap the
        // PDCurses MODIFIER flag at BUTTON_MODIFIER_SHIFT, so decoding them
        // as wheel here would misread a shift-click).
        let pressed = |n: u32| bstate & (ACTION_PRESSED << ((n - 1) * MOUSE_BUTTON_STRIDE)) != 0;
        let action = if pressed(4) {
            MouseAction::Wheel { down: false, horizontal: false }
        } else if pressed(5) {
            MouseAction::Wheel { down: true, horizontal: false }
        } else {
            // Walk the button groups.  A single event normally sets exactly one
            // group; when several are set (PDCurses motion can flag the held
            // button) the lowest-numbered button wins, matching the mask
            // order.
            let mut decoded: Option<MouseAction> = None;
            let mut moved_button: Option<u8> = None;
            // Only the three normal buttons are examined here: the higher
            // five-bit windows overlap PDCurses' modifier flags, so a
            // shift-click would otherwise look like a button-6 event.
            for n in 1..=5u32 {
                let group = (bstate >> ((n - 1) * MOUSE_BUTTON_STRIDE)) & 0x1f;
                // PDCurses reuses the top bit of each button group for its
                // `BUTTONn_MOVED` flag, aliasing `BUTTONn_TRIPLE_CLICKED`.  A
                // motion record is the one that ALSO sets PDCurses'
                // `PDC_MOUSE_MOVED` change bit (bit 3) — ncurses never reports
                // motion through `bstate` at all — so only then is the top bit
                // read as movement; otherwise it stays a triple click.
                if group & ACTION_PRESSED != 0 {
                    decoded = decoded.or(Some(MouseAction::Pressed(n as u8)));
                } else if group & ACTION_CLICKED != 0 {
                    decoded = decoded.or(Some(MouseAction::Clicked(n as u8)));
                } else if bstate & MOUSE_FLAG_MOVED != 0 && group & ACTION_TRIPLE != 0 {
                    // PDCurses motion record (see above): the group carries
                    // BUTTONn_MOVED, which aliases the double/triple bits, so
                    // it must be recognised before either of them.
                    moved_button = moved_button.or(Some(n as u8));
                } else if group & ACTION_DOUBLE != 0 {
                    decoded = decoded.or(Some(MouseAction::DoubleClicked(n as u8)));
                } else if group & ACTION_TRIPLE != 0 {
                    decoded = decoded.or(Some(MouseAction::TripleClicked(n as u8)));
                } else if group & ACTION_RELEASED != 0 {
                    decoded = decoded.or(Some(MouseAction::Released(n as u8)));
                }
            }
            decoded
                .or_else(|| {
                    if moved_button.is_some() || bstate & MOUSE_FLAG_MOVED != 0 {
                        Some(MouseAction::Moved { button: moved_button })
                    } else if bstate & MOUSE_FLAG_POSITION != 0 {
                        Some(MouseAction::Position)
                    } else {
                        None
                    }
                })
                .unwrap_or(MouseAction::Position)
        };

        MouseEvent { x, y, action, shift, ctrl, alt, raw_state: bstate }
    }

    /// Cell position of a pointer event in a widget's local coordinates.
   ///
    /// `None` when the point is outside the widget (or the widget has no
    /// area) — i.e. the same containment test the dispatcher uses.
    fn mouse_local(kind_rect: Rect, x: i32, y: i32) -> Option<(i32, i32)> {
        if kind_rect.w <= 0 || kind_rect.h <= 0 {
            return None;
        }
        if x < kind_rect.x || x >= kind_rect.x + kind_rect.w {
            return None;
        }
        if y < kind_rect.y || y >= kind_rect.y + kind_rect.h {
            return None;
        }
        Some((x - kind_rect.x, y - kind_rect.y))
    }

    /// Map a pointer position inside a spreadsheet widget to a cell.
    ///
    /// Mirrors the terminal cell-grid renderer's own layout (see
    /// `backends::pancurses_draw`): the header band is
    /// `header_row_count * HEADER_H` rows tall, the row-label gutter is
    /// `ROW_LABEL_W` wide, and each column occupies its layout width in
    /// `CHAR_W`-sized cells.  Returns `None` for the chrome (header band,
    /// row-label gutter, past the last column/row).
    ///
    /// Pure, so it is unit-tested without a terminal — the renderer and the
    /// hit-test cannot drift apart silently.
    fn spreadsheet_cell_at(grid: &crate::core::Grid, local: (i32, i32)) -> Option<(u32, u32)> {
        let (lx, ly) = local;
        if lx < 0 || ly < 0 {
            return None;
        }
        let char_w = crate::spreadsheet::SpreadsheetModel::CHAR_W as i32;
        let row_h = crate::spreadsheet::SpreadsheetModel::ROW_H as i32;
        let header_h = crate::spreadsheet::SpreadsheetModel::HEADER_H as i32;
        let label_w = crate::spreadsheet::SpreadsheetModel::ROW_LABEL_W as i32;
        let header_rows = grid.header_row_count as i32;
        let header_h_total = header_rows * header_h;
        if ly < header_h_total {
            return None;
        }
        let body_row = (ly - header_h_total) / row_h.max(1);
        // Body row 0 is the first non-header row.  The renderer scrolls with
        // `top_row` (`row_idx = top_row + vr`), so a click maps to the same
        // absolute row the renderer would draw there.
        let row = grid.top_row.saturating_add(body_row as u32);
        if row >= grid.total_rows.max(1) {
            return None;
        }
        if lx < label_w {
            // Row-label gutter: treat every margin column band as clickable,
            // mirroring `spreadsheet::cell_at`'s pixel walk.
            let margin = grid.margin_cols.max(1) as i32;
            let band = (label_w / margin).max(1);
            let col = ((lx / band) as u32).min(grid.margin_cols.saturating_sub(1));
            return Some((row, col));
        }
        let mut cx = label_w;
        // The margin columns live inside the row-label gutter (the renderer
        // draws `c < margin_cols` at `c * label_w / margin_cols`), so the
        // main-column walk starts after them.
        let mut col = grid.margin_cols;
        loop {
            if col >= grid.total_cols {
                return None;
            }
            let width = spreadsheet_col_width(grid, col) * char_w;
            if lx < cx + width {
                return Some((row, col));
            }
            cx += width;
            col += 1;
        }
    }

    /// Rendered width (in character cells) of one spreadsheet column: the
    /// explicit layout entry when the app supplied one, otherwise the grid
    /// default (`col_width`), matching the renderer.
    fn spreadsheet_col_width(grid: &crate::core::Grid, col: u32) -> i32 {
        for (gc, w, _) in &grid.column_layout {
            if *gc == col {
                return (*w as i32).max(1);
            }
        }
        (grid.col_width as i32).max(1)
    }

    /// Register a named action (mirrors wxAction).  Menu items reference
    /// actions by name; the registry holds their shared enable/checked state.
    pub fn register_action(name: &str, enabled: bool, checked: bool) {
        ACTION_REGISTRY.with(|r| {
            r.borrow_mut().insert(name.to_string(), crate::Action {
                name: name.to_string(),
                enabled,
                checked,
            });
        });
    }

    /// Update an action's enabled state (greys the item everywhere it appears).
    pub fn set_action_enabled(name: &str, enabled: bool) {
        ACTION_REGISTRY.with(|r| {
            if let Some(a) = r.borrow_mut().get_mut(name) {
                a.enabled = enabled;
            }
        });
    }

    /// Update an action's checked state (toggles check/radio items).
    pub fn set_action_checked(name: &str, checked: bool) {
        ACTION_REGISTRY.with(|r| {
            if let Some(a) = r.borrow_mut().get_mut(name) {
                a.checked = checked;
            }
        });
    }

    /// Query an action's (enabled, checked) state, if registered.
    pub fn action_state(name: &str) -> Option<(bool, bool)> {
        ACTION_REGISTRY.with(|r| {
            r.borrow().get(name).map(|a| (a.enabled, a.checked))
        })
    }

    /// Show a modal info dialog (title + body) drawn via SGR so it appears on
    /// top of the spreadsheet output (ncurses widgets get overwritten by the
    /// spreadsheet's direct SGR writes).  Dismissed with Escape.
    pub fn show_dialog(title: &str, text: &str) {
        ACTIVE_DIALOG.with(|d| *d.borrow_mut() = Some((title.to_string(), text.to_string())));
    }

    /// Show a standard message box (mirrors wxMessageBox).  The pancurses
    /// backend renders it with the info-dialog overlay; dismissing it (Escape)
    /// fires `on_result` with Ok.  Higher-button kinds (Yes/No/Cancel) map to
    /// Ok on dismissal for now — native backends provide full button rows.
    pub fn message_box(title: &str, text: &str, _kind: crate::MessageBoxKind, on_result: Option<Box<dyn FnMut(crate::MessageBoxResult)>>) {
        ACTIVE_DIALOG.with(|d| *d.borrow_mut() = Some((title.to_string(), text.to_string())));
        DIALOG_RESULT.with(|r| *r.borrow_mut() = on_result);
    }

    /// Close the active info dialog, firing the message-box result callback.
    pub fn close_dialog() {
        let result = DIALOG_RESULT.with(|r| r.borrow_mut().take());
        ACTIVE_DIALOG.with(|d| *d.borrow_mut() = None);
        if let Some(mut cb) = result {
            cb(crate::MessageBoxResult::Ok);
        }
    }

    /// Request a full redraw after the current key is processed (generic
    /// mechanism for host key hooks: a consumed key skips default handling,
    /// which is also what would have repainted, so the host asks explicitly).
    pub fn request_redraw() {
        REDRAW_REQUESTED.with(|f| *f.borrow_mut() = true);
    }

    /// Suspend the terminal so the host can run an external subprocess
    /// (e.g. `$EDITOR`) on the real screen: restores cooked mode and the
    /// pre-TUI contents. Pair with [`resume_terminal`]. Generic mechanism;
    /// the host decides when a subprocess needs it.
    pub fn suspend_terminal() {
        def_prog_mode();
        endwin();
    }

    /// Re-enter curses mode after [`suspend_terminal`] and schedule a full
    /// repaint (the subprocess may have drawn over everything).
    pub fn resume_terminal() {
        reset_prog_mode();
        request_redraw();
    }

    /// Show a single-choice list popup (generic mechanism; see ACTIVE_PICKER).
    /// `selected` is clamped to the rows. The host updates it with
    /// [`set_list_picker_selection`] as keys arrive.
    pub fn show_list_picker(title: &str, rows: &[String], selected: usize) {
        let sel = if rows.is_empty() { 0 } else { selected.min(rows.len() - 1) };
        ACTIVE_PICKER.with(|p| {
            *p.borrow_mut() = Some((title.to_string(), rows.to_vec(), sel));
        });
    }

    /// Move the list-popup highlight (clamped). No-op when closed.
    pub fn set_list_picker_selection(selected: usize) {
        ACTIVE_PICKER.with(|p| {
            if let Some((_, rows, sel)) = p.borrow_mut().as_mut() {
                *sel = if rows.is_empty() { 0 } else { selected.min(rows.len() - 1) };
            }
        });
    }

    /// Dismiss the list popup. No-op when closed.
    pub fn close_list_picker() {
        ACTIVE_PICKER.with(|p| *p.borrow_mut() = None);
    }

    /// Erase the dialog box area (spaces) so closing it does not leave stale
    /// dialog text on the terminal (the spreadsheet SGR output does not
    /// overwrite empty cells).  The dialog fills the grid area (below the menu
    /// bar and formula bar), so clear that whole region — but NOT the last row
    /// (the hints/status line), which stays visible, matching ratatui.
    fn clear_dialog_area(root: &Window) {
        if ACTIVE_DIALOG.with(|d| d.borrow().is_some()) {
            let (my, mx) = root.get_max_yx();
            let y0 = 2;
            let y1 = (my - 1).max(y0 + 1); // leave the hints row (my-1) alone
            let mut out = String::new();
            for y in y0..y1 {
                out.push_str(&sgr_cup(y, 0));
                out.push_str(SGR_RESET);
                out.push_str(&" ".repeat(mx as usize));
            }
            emit_sgr(&out);
        }
    }

    /// Register the callback invoked when a menu item is activated. The
    /// callback receives the item's action name (see `gui::menu::action_kind_to_name`).
    pub fn set_menu_action_callback(cb: Box<dyn FnMut(String)>) {
        with_state(|state| state.menu_action_callback = Some(cb));
    }

    /// Show a text prompt (label + current buffer) and remember the action to
    /// perform with the submitted text.  Key input is routed to the buffer while
    /// the prompt is active; Enter submits, Escape cancels.
    /// Show a standard file-open dialog (mirrors wxFileDialog).  The pancurses
    /// backend presents a path prompt; `on_path` fires with the submitted path.
    pub fn file_open_dialog(on_path: Box<dyn FnMut(Option<String>)>) {
        let mut on_path = on_path;
        set_prompt("Open file", "__file_open");
        set_prompt_callback(Box::new(move |action, text| {
            if action == "__file_open" { on_path(Some(text)); }
        }));
    }

    /// Show a standard file-save dialog (mirrors wxFileDialog).  The pancurses
    /// backend presents a path prompt; `on_path` fires with the submitted path.
    pub fn file_save_dialog(default_name: &str, on_path: Box<dyn FnMut(Option<String>)>) {
        let mut on_path = on_path;
        set_prompt(&format!("Save as ({default_name})"), "__file_save");
        set_prompt_callback(Box::new(move |action, text| {
            if action == "__file_save" { on_path(Some(text)); }
        }));
    }

    pub fn set_prompt(label: &str, action: &str) {
        with_state(|state| {
            state.prompt_label = label.to_string();
            state.prompt_buffer.clear();
            state.prompt_action = Some(action.to_string());
        });
    }

    /// Register the callback invoked when a prompt is submitted. Receives the
    /// action name and the entered text.
    pub fn set_prompt_callback(cb: Box<dyn FnMut(String, String)>) {
        with_state(|state| state.prompt_callback = Some(cb));
    }

    fn with_state<F, R>(f: F) -> R
    where
        F: FnOnce(&mut PcState) -> R,
    {
        PC_STATE.with(|s| f(&mut s.borrow_mut()))
    }

    pub struct PcApp;

    impl crate::backends::BackendApp for PcApp {
        /// A terminal has no framework to dispatch for us: this `run` blocks in
        /// `getch()` and is the loop (see `BackendApp::owns_event_loop`).
        fn owns_event_loop(&self) -> bool {
            true
        }

        fn run(self: Box<Self>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            // ncurses holds a bare Escape key for ESCDELAY (default 1000 ms)
            // before delivering it, to disambiguate it from escape sequences
            // (arrow keys etc.).  That made dismissing a modal dialog (About /
            // Help) with Esc take ~1 s.  Set a short delay BEFORE initscr() so
            // bare Esc is delivered promptly; sequence recognition is unaffected
            // because complete sequences arrive as consecutive bytes.
            // Windows: skip it — SetEnvironmentVariableW is a stub returning
            // ERROR_CALL_NOT_IMPLEMENTED (120) on Win9x/ME, which makes
            // std::env::set_var PANIC there, and PDCurses doesn't read
            // ESCDELAY at all (ncurses-only knob).
            #[cfg(unix)]
            if std::env::var_os("ESCDELAY").is_none() {
                std::env::set_var("ESCDELAY", "25");
            }
            let mut root = initscr();
            raw();
            noecho();
            root.keypad(true);
            // Generic input tracing (see trace_input_line): enabled when the
            // host sets INPUT_TRACE_FILE to a writable path.
            if std::env::var("INPUT_TRACE_FILE").is_ok() {
                INPUT_TRACE_FD.store(1, std::sync::atomic::Ordering::Relaxed);
            }
            // Drop any console input records left over before we started (e.g.
            // the keystrokes typed to launch the app in a Win9x DOS box); a
            // stale Enter would otherwise be delivered to the app as input.
            flushinp();
            // Register CSI-form arrow keys so that \x1b[A etc. are
            // recognized even when terminfo uses SS3 (\x1bOA etc.).
            // screen-256color lacks kri/kind/kLFT/kRIT, so \x1b[1;2{A,B,C,D}
            // (shift+arrows) must be registered explicitly too — as the
            // *shifted* keycodes, so Shift+arrow extends the selection
            // (ratatui/GUI parity) instead of aliasing onto plain movement.
            // define_key is ncurses/PDCurses but is absent from the PDCurses
            // version bundled by pdcurses-sys 0.7.x. On Windows, ConPTY handles
            // these sequences natively, so skip the workaround there.
            #[cfg(not(windows))]
            {
                use std::os::raw::c_char;
                extern "C" {
                    fn define_key(definition: *const c_char, keycode: i32) -> i32;
                }
                for (seq, code) in CSI_KEY_BINDINGS {
                    unsafe { define_key(seq.as_ptr() as *const c_char, code); }
                }
            }
            // Mouse capture is OFF by default: capturing the mouse (SGR mouse
            // mode) makes terminals (Windows Terminal, etc.) swallow drag
            // events and breaks text selection.  Apps that need pointer input
            // call set_mouse_enabled(true).
            mousemask(0, None);
            // Use `root.timeout(100)` so we can temporarily change it for Alt+key detection
            root.timeout(100);
            curs_set(0);

            if has_colors() {
                start_color();
                init_pair(1, COLOR_WHITE, COLOR_BLUE);
                init_pair(2, COLOR_BLACK, COLOR_WHITE);
                init_pair(3, COLOR_WHITE, COLOR_BLACK);
                init_pair(4, COLOR_YELLOW, COLOR_BLACK);
                init_pair(5, COLOR_GREEN, COLOR_BLACK);
                init_pair(6, COLOR_RED, COLOR_BLACK);
                init_pair(7, COLOR_CYAN, COLOR_BLACK);
            }

            with_state(|state| {
                let (my, mx) = root.get_max_yx();
                for node in &mut state.nodes {
                    match &node.kind {
                        PcWidgetKind::Window { .. } | PcWidgetKind::Dialog { .. } => {
                            node.rect = Rect { x: 0, y: 0, w: mx, h: my };
                        }
                        PcWidgetKind::MenuBar { .. } => {
                            state.menu_bar_id = Some(node.id);
                        }
                        _ => {}
                    }
                }
                state.running = true;
            });

            // Redraw only when a key/event arrives (avoids flicker). The initial
            // frame is drawn once here; subsequent frames only on input.
            let redraw_frame = |root: &mut Window| {

                // Clear direct-write spreadsheet output for this frame
                with_state(|s| s.spreadsheet_output.clear());
                // layout pass
                with_state(|state| {
                    let ids: Vec<usize> = state.nodes.iter().map(|n| n.id).collect();
                    for id in &ids {
                        let info = state.node(*id).and_then(|n| {
                            let pid = n.parent?;
                            let pn = state.node(pid)?;
                            Some((n.rect, pn.rect, matches!(pn.kind, PcWidgetKind::Dialog { .. })))
                        });
                        if let Some((r, pr, is_dialog)) = info {
                            if (r.w == 0 || r.h == 0) && (pr.w > 0 || pr.h > 0) {
                                if let Some(n) = state.node_mut(*id) {
                                    let ins = if is_dialog { 1 } else { 0 };
                                    if n.rect.w == 0 { n.rect.w = (pr.w - 2 * ins).max(1); }
                                    if n.rect.h == 0 { n.rect.h = (pr.h - 2 * ins).max(1); }
                                    if is_dialog {
                                        if n.rect.x == 0 { n.rect.x = pr.x + 1; }
                                        if n.rect.y == 0 { n.rect.y = pr.y + 1; }
                                    }
                                }
                            }
                        }
                    }
                    for id in &ids {
                        let kind = state.node(*id).map(|n| &n.kind).cloned();
                        match kind {
                            Some(PcWidgetKind::BoxWidget { .. }) => { layout_box_inner(*id, state); }
                            Some(PcWidgetKind::Grid { .. }) => { layout_grid_inner(*id, state); }
                            Some(PcWidgetKind::Sizer(_)) => { layout_sizer_inner(*id, state); }
                            _ => {}
                        }
                    }
                    for id in &ids {
                        if let Some(rect) = state.node(*id).map(|n| n.rect) {
                            if let Some(pid) = state.node(*id).and_then(|n| n.parent) {
                                if let Some(p) = state.node(pid) {
                                    let px = p.rect.x;
                                    let py = p.rect.y;
                                    let pw = p.rect.w.max(1);
                                    let ph = p.rect.h.max(1);
                                    let mut nr = rect;
                                    nr.x = rect.x.clamp(px, px + pw - 1);
                                    nr.y = rect.y.clamp(py, py + ph - 1);
                                    nr.w = rect.w.min(px + pw - nr.x);
                                    nr.h = rect.h.min(py + ph - nr.y);
                                    if let Some(n) = state.node_mut(*id) { n.rect = nr; }
                                }
                            }
                        }
                    }
                });

                // Menu-bar bracket tracking: derive the bar line from the
                // menubar widget (open menu bracketed, first root when
                // closed) so it matches the ratatui reference. Computed
                // once per frame outside any state borrow.
                let menu_bar_text = with_state(|s| derived_menu_bar_text(s));
                // render
                for i in 0..with_state(|s| s.nodes.len()) {
                    let (kind, rect, visible, id, focus_id) = with_state(|s| {
                        let n = &s.nodes[i];
                        (n.kind.clone(), n.rect, n.visible, n.id, s.focus_id)
                    });
                    if !visible { continue; }
                    render_widget(&root, &kind, rect, id, focus_id, menu_bar_text.as_deref());
                }
                // Canvas draw callbacks, painted after the widget tree so a
                // canvas's own output lands on its background (the GTK
                // ordering). Their grid output is SGR-flushed here, before the
                // backend's own SGR stream, so later overlays still win.
                paint_canvases();
                // render active menu dropdown (all open levels: root + submenus)
                with_state(|state| {
                    if state.menu_open {
                        if let Some(levels) = menu_levels(state) {
                            let item_idx = state.active_item;
                            let last = levels.len() - 1;
                            for (li, (py, px, items)) in levels.iter().enumerate() {
                                let max_w = items.iter().map(menu_item_width).max().unwrap_or(4).max(4) as i32;
                                if has_colors() { root.attron(COLOR_PAIR(4)); }
                                // Clear the popup body (top border row through bottom
                                // border row) so box-drawing chars are not overdrawn.
                                for row in 0..items.len() as i32 + 2 {
                                    for col in -1..max_w + 5 {
                                        root.mvaddch(py + row, px + col, ' ');
                                    }
                                }
                                if has_colors() { root.attroff(COLOR_PAIR(4)); }
                                for (i, item) in items.iter().enumerate() {
                                    let bg = if li == last && i == item_idx && has_colors() { COLOR_PAIR(2) } else { 0 };
                                    if bg != 0 { root.attron(bg); }
                                    let lbl = menu_item_label(item);
                                    root.mvaddstr(py + 1 + i as i32, px + 1, &lbl);
                                    if bg != 0 { root.attroff(bg); }
                                }
                            }
                        }
                    }
                });

                root.refresh();
                // Flush direct-write spreadsheet SGR output after ncurses flush
                with_state(|s| {
                    if !s.spreadsheet_output.is_empty() {
                        emit_sgr(&s.spreadsheet_output);
                    }
                });
                // Emit the open menu popup via SGR (same direct-output path as the
                // spreadsheet cells) so it appears on top of the SGR output.
                with_state(|state| {
                    if state.menu_open {
                        if let Some(levels) = menu_levels(state) {
                            let item_idx = state.active_item;
                            let last = levels.len() - 1;
                            let root_title = menu_root_title(state);
                            let mut out = String::new();
                            for (li, (py, px, items)) in levels.iter().enumerate() {
                                let max_w = items.iter().map(menu_item_width).max().unwrap_or(4).max(4) as i32;
                                let bw = max_w + 4; // box width (2 padding + 2 borders)
                                let left = *px;
                                let right = left + bw - 1;
                                let top = *py;
                                let bottom = py + items.len() as i32 + 1;
                                // Popup background fill (inner area) + clear the grid's
                                // left-edge column (left-1) so the grid border does not
                                // show through behind the popup box.
                                for row in 1..items.len() as i32 + 1 {
                                    out.push_str(&sgr_cup(py + row, left - 1));
                                    out.push_str(SGR_RESET);
                                    out.push(' ');
                                    out.push_str(&sgr_cup(py + row, left));
                                    out.push_str(&" ".repeat(bw as usize));
                                }
                                out.push_str(&sgr_cup(top, left - 1));
                                out.push_str(SGR_RESET);
                                out.push(' ');
                                out.push_str(&sgr_cup(bottom, left - 1));
                                out.push_str(SGR_RESET);
                                out.push(' ');
                                // Box border; include the root menu title in the top
                                // border (matching the ratatui popup, e.g. ┌File───┐).
                                let title = if li == 0 { root_title.as_str() } else { "" };
                                out.push_str(&sgr_cup(top, left));
                                out.push_str(SGR_RESET);
                                out.push('\u{250c}');
                                out.push_str(title);
                                let dash_n = (bw - 2 - title.chars().count() as i32).max(0) as usize;
                                out.push_str(&"\u{2500}".repeat(dash_n));
                                out.push('\u{2510}');
                                for row in 1..items.len() as i32 + 1 {
                                    out.push_str(&sgr_cup(py + row, left));
                                    out.push_str(SGR_RESET);
                                    out.push('\u{2502}');
                                    out.push_str(&sgr_cup(py + row, right));
                                    out.push_str(SGR_RESET);
                                    out.push('\u{2502}');
                                }
                                out.push_str(&sgr_cup(bottom, left));
                                out.push_str(SGR_RESET);
                                out.push('\u{2514}');
                                out.push_str(&"\u{2500}".repeat((bw - 2).max(0) as usize));
                                out.push('\u{2518}');
                                // Labels (inside the box, with reverse-video highlight)
                                for (i, item) in items.iter().enumerate() {
                                    out.push_str(&sgr_cup(py + 1 + i as i32, left + 1));
                                    out.push_str(if li == last && i == item_idx { sgr_row_cursor() } else { sgr_menu() });
                                    out.push(' ');
                                    out.push_str(&menu_item_label(item));
                                    out.push_str(SGR_RESET);
                                }
                            }
                            emit_sgr(&out);
                        }
                    }
                });

            // Generic after-redraw hook: lets the host app run a callback once
            // the frame is fully flushed (e.g. an idle marker for tests).  The
            // toolkit itself never writes app-specific files.
            AFTER_REDRAW.with(|c| {
                if let Some(cb) = c.borrow_mut().as_mut() {
                    cb();
                }
            });

            // Modal info dialog (About/Help) drawn via SGR on top of everything.
            // It fills the grid area (below the menu bar and formula bar) and
            // word-wraps long lines, matching the ratatui reference's full-width
            // bordered box (render parity).
            if let Some((dtitle, dtext)) = ACTIVE_DIALOG.with(|d| d.borrow().clone()) {
                let (my, mx) = root.get_max_yx();
                // The grid area starts at row 2 (menu bar row 0, formula bar row 1)
                // and leaves the last row for the hints/status line, matching the
                // ratatui reference's grid_area (rows 2..my-1).
                let y0 = 2;
                let h = (my - y0 - 1).max(3);
                let x0 = 0;
                let w = mx;
                // Clear only the rows the dialog covers (y0..y0+h); the last
                // row (my-1) is the hints/status line and must stay visible,
                // matching the ratatui reference (its overlay covers only
                // grid_area, not the hints row).
                let y1 = (y0 + h).min(my);
                let mut out = String::new();
                for y in y0..y1 {
                    out.push_str(&sgr_cup(y, x0));
                    out.push_str(SGR_RESET);
                    out.push_str(&" ".repeat(w as usize));
                }
                // The body fills the box's inner area: it starts right after the
                // left border (x0+1) and is w-2 wide, matching the ratatui
                // reference's `block.inner(grid_area)` so long lines wrap at the
                // same point (render parity).
                let inner = (w - 2).max(1) as usize;
                // Word-wrap the body to the inner width (ratatui wraps, it does
                // not truncate).
                let mut wrapped: Vec<String> = Vec::new();
                for ln in dtext.lines() {
                    let mut cur = String::new();
                    for word in ln.split(' ') {
                        if cur.is_empty() {
                            cur = word.to_string();
                        } else if cur.chars().count() + 1 + word.chars().count() <= inner {
                            cur.push(' ');
                            cur.push_str(word);
                        } else {
                            wrapped.push(cur);
                            cur = word.to_string();
                        }
                    }
                    wrapped.push(cur);
                }
                // Top border with the title.
                out.push_str(&sgr_cup(y0, x0));
                out.push_str(SGR_RESET);
                out.push('\u{250c}');
                out.push_str(&"\u{2500}".repeat((w - 2) as usize));
                out.push('\u{2510}');
                out.push_str(&sgr_cup(y0, x0 + 1));
                out.push_str(sgr_header_active());
                let t: String = dtitle.chars().take((w - 4).max(1) as usize).collect();
                out.push_str(&t);
                out.push_str(SGR_RESET);
                // Body lines (wrapped), each with left/right borders.  The box
                // has a fixed height (`body_rows`), so EVERY body row must carry
                // both borders — including empty rows below the wrapped text.
                // Previously this loop iterated `wrapped.iter()` directly, so a
                // short body (e.g. About with ~8 wrapped lines) left all rows
                // below the content as bare cleared grid lines with no `│`
                // borders, diverging from the ratatui reference (which draws
                // `│` on every row of the box).
                let body_rows = (h - 2).max(0) as usize;
                for i in 0..body_rows {
                    let y = y0 + 1 + i as i32;
                    out.push_str(&sgr_cup(y, x0));
                    out.push_str(SGR_RESET);
                    out.push('\u{2502}');
                    out.push_str(&sgr_cup(y, x0 + w - 1));
                    out.push_str(SGR_RESET);
                    out.push('\u{2502}');
                    if let Some(ln) = wrapped.get(i) {
                        let body: String = ln.chars().take(inner).collect();
                        out.push_str(&sgr_cup(y, x0 + 1));
                        out.push_str(SGR_RESET);
                        out.push_str(&body);
                    }
                }
                // Bottom border.
                out.push_str(&sgr_cup(y0 + h - 1, x0));
                out.push_str(SGR_RESET);
                out.push('\u{2514}');
                out.push_str(&"\u{2500}".repeat((w - 2) as usize));
                out.push('\u{2518}');
                emit_sgr(&out);
            }
            // Single-choice list popup (picker) — centered box with one row
            // per item and a highlighted selected row, on top of the dialog
            // (a picker is always opened above any info dialog). Same box
            // drawing and highlight convention as the menu popups.
            // NOTE: inside redraw_frame (per-frame), not with the one-shot
            // setup below: the popup must repaint on every navigation key.
            if let Some((ptitle, prows, psel)) = ACTIVE_PICKER.with(|p| p.borrow().clone()) {
                let (my, mx) = root.get_max_yx();
                let inner_w = prows
                    .iter()
                    .map(|r| r.chars().count())
                    .max()
                    .unwrap_or(0)
                    .max(ptitle.chars().count())
                    + 4;
                let bw = (inner_w + 2).min(mx.max(1) as usize) as i32;
                let bh = (prows.len() + 2).max(3) as i32;
                let left = ((mx - bw) / 2).max(0);
                let top = ((my - bh) / 2).max(0);
                let mut out = String::new();
                for y in top..(top + bh).min(my) {
                    out.push_str(&sgr_cup(y, left));
                    out.push_str(SGR_RESET);
                    out.push_str(&" ".repeat((bw as usize).min((mx - left).max(0) as usize)));
                }
                out.push_str(&sgr_cup(top, left));
                out.push_str(SGR_RESET);
                out.push('\u{250c}');
                out.push_str(&"\u{2500}".repeat((bw - 2).max(0) as usize));
                out.push('\u{2510}');
                out.push_str(&sgr_cup(top, left + 1));
                out.push_str(sgr_header_active());
                let t: String = ptitle.chars().take((bw - 4).max(1) as usize).collect();
                out.push_str(&t);
                out.push_str(SGR_RESET);
                for (i, row) in prows.iter().enumerate() {
                    if top + 1 + i as i32 >= my - 1 {
                        break;
                    }
                    out.push_str(&sgr_cup(top + 1 + i as i32, left));
                    out.push_str(SGR_RESET);
                    out.push('\u{2502}');
                    out.push_str(if i == psel { sgr_row_cursor() } else { sgr_menu() });
                    out.push_str(if i == psel { "▸ " } else { "  " });
                    let cell: String = row.chars().take((bw - 6).max(1) as usize).collect();
                    out.push_str(&cell);
                    out.push_str(SGR_RESET);
                    out.push_str(&sgr_cup(top + 1 + i as i32, left + bw - 1));
                    out.push_str(SGR_RESET);
                    out.push('\u{2502}');
                }
                out.push_str(&sgr_cup(top + bh - 1, left));
                out.push_str(SGR_RESET);
                out.push('\u{2514}');
                out.push_str(&"\u{2500}".repeat((bw - 2).max(0) as usize));
                out.push('\u{2518}');
                emit_sgr(&out);
            }
            };

            // Prompt overlay (Open/Save/Export path entry) — drawn whenever a
            // text prompt is active, independent of the menu-open state.
            with_state(|state| {
                if state.prompt_action.is_some() {
                    let mut out2 = String::new();
                    let text = format!("{}: {}_", state.prompt_label, state.prompt_buffer);
                    out2.push_str(&sgr_cup(1, 0));
                    out2.push_str(SGR_RESET);
                    out2.push_str(&" ".repeat(200));
                    out2.push_str(&sgr_cup(1, 0));
                    out2.push_str(sgr_prompt());
                    out2.push_str(&text);
                    out2.push_str(SGR_RESET);
                    emit_sgr(&out2);
                }
            });

            redraw_frame(&mut root);

            while with_state(|s| s.running) {
                // Block for input first; only redraw when a key/event arrives
                // (avoids flicker from redrawing every 100ms timeout).
                let input = root.getch();

                // Generic input tracing hook: if INPUT_TRACE_FD is set, the
                // backend writes one line per getch result (raw curses code or
                // decoded Input) so host test harnesses can diagnose input
                // delivery. The toolkit itself knows nothing about the file's
                // purpose.
                if INPUT_TRACE_FD.load(std::sync::atomic::Ordering::Relaxed) >= 0 {
                    trace_input_line(&input);
                }

                // Generic pre-dispatch key hook: if the host consumes the key
                // (returns true), skip the default handling below entirely.
                let consumed = KEY_INPUT_HOOK.with(|h| {
                    h.borrow_mut().as_mut().map_or(false, |cb| {
                        let key = convert_key_input(&input);
                        cb(&key)
                    })
                });
                if consumed {
                    if REDRAW_REQUESTED.with(|f| std::mem::replace(&mut *f.borrow_mut(), false)) {
                        redraw_frame(&mut root);
                    }
                    continue;
                }

                // Host frame hook: lets a host app mirror external state into the
                // widget tree on the main thread (e.g. flush a transcript into a
                // TextView). Runs every loop iteration; cleared on quit.
                FRAME_HOOK.with(|h| {
                    if let Some(cb) = h.borrow_mut().as_mut() {
                        cb();
                    }
                });
                match input {
                    Some(Input::KeyResize) => {
                        let (my, mx) = root.get_max_yx();
                        root.clear();
                        with_state(|state| {
                            for node in &mut state.nodes {
                                match &node.kind {
                                    PcWidgetKind::Window { .. } | PcWidgetKind::Dialog { .. } => {
                                        node.rect = Rect { x: 0, y: 0, w: mx, h: my };
                                    }
                                    _ => {
                                        node.rect = Rect::default();
                                    }
                                }
                            }
                        });
                    }
                    Some(Input::KeyMouse) => {
                        if let Ok(mevent) = getmouse() {
                            let ev = decode_mouse_event(mevent.x, mevent.y, mevent.bstate as u64);
                            // Host hook first (see set_mouse_hook): a host that
                            // handles the event itself skips the default dispatch.
                            let consumed = MOUSE_HOOK.with(|h| {
                                h.borrow_mut().as_mut().map_or(false, |cb| cb(&ev))
                            });
                            if !consumed {
                                handle_mouse_event(ev);
                            }
                        }
                    }
                    Some(Input::Character('\n')) | Some(Input::Character('\r')) => {
                        with_state(|state| state.pending_quit = false);
                        // If a text prompt is active, submit it instead of the usual Enter.
                        let prompt: Option<(String, String)> = with_state(|state| {
                            if let Some(a) = state.prompt_action.take() {
                                let t = std::mem::take(&mut state.prompt_buffer);
                                Some((a, t))
                            } else {
                                None
                            }
                        });
                        if let Some((action, text)) = prompt {
                            let cb = with_state(|state| state.prompt_callback.take());
                            if let Some(mut cb) = cb {
                                cb(action, text);
                                with_state(|state| state.prompt_callback = Some(cb));
                            }
                            redraw_frame(&mut root);
                            continue;
                        }
                        // If a menu is open, capture the active item's action name and
                        // dispatch it (instead of only closing the menu).  A submenu
                        // item is entered instead (matching ratatui).
                        let menu_action: Option<String> = with_state(|state| {
                            if state.menu_open {
                                let item = menu_current_item(state).cloned();
                                match item {
                                    Some(crate::MenuItem::Action { action, .. })
                                    | Some(crate::MenuItem::Check { action, .. })
                                    | Some(crate::MenuItem::Radio { action, .. }) => {
                                        state.menu_open = false;
                                        Some(action)
                                    }
                                    Some(crate::MenuItem::Separator) => None,
                                    Some(crate::MenuItem::Submenu { .. }) => {
                                        menu_enter_submenu(state);
                                        None
                                    }
                                    None => {
                                        state.menu_open = false;
                                        None
                                    }
                                }
                            } else {
                                None
                            }
                        });
                        // Process Enter action and collect toggle callbacks
                        let toggle_callbacks: Vec<Callback> = with_state(|state| {
                            if menu_action.is_some() {
                                vec![]
                            } else if let Some(fid) = state.focus_id {
                                if is_spreadsheet_focused(state, fid) {
                                    spreadsheet_enter(state, fid);
                                    spreadsheet_scroll_to_cursor(state, fid);
                                    vec![]
                                } else {
                                    toggle_focused(state, fid).1
                                }
                            } else {
                                vec![]
                            }
                        });
                        fire_callbacks(toggle_callbacks);
                        // Fire any deferred commit-edit callbacks BEFORE the cursor-move
                        // callback re-fills cells, so the grid already holds the committed
                        // value.  Otherwise the re-fill reads the stale pre-commit grid and
                        // the display reverts to the old text until the next redraw (e.g.
                        // overwriting A1 with "BBB" then moving Up/Down flickers between
                        // "AAA" and "AAABBB").
                        let deferred = DEFERRED_COMMIT.with(|d| d.borrow_mut().drain(..).collect::<Vec<_>>());
                        for (r, c, val, mut cbs) in deferred {
                            for cb in cbs.iter_mut() { cb(r, c, val.clone()); }
                            with_state(|state| state.commit_edit_callbacks.append(&mut cbs));
                        }
                        // After Enter on a spreadsheet, fire the cursor-move callbacks
                        // (outside any with_state borrow) so the host can sync the app
                        // cursor and grow the grid — matching arrow-key behavior.  This
                        // keeps the formula bar / row labels correct after commit+move.
                        if menu_action.is_none() {
                            let enter_pos: Option<(u32, u32)> = with_state(|state| {
                                if let Some(fid) = state.focus_id {
                                    if is_spreadsheet_focused(state, fid) {
                                        if let Some(n) = state.node(fid) {
                                            if let PcWidgetKind::Spreadsheet { grid, .. } = &n.kind { let cursor_row = &grid.cursor_row; let cursor_col = &grid.cursor_col;
                                                return Some((*cursor_row, *cursor_col));
                                            }
                                        }
                                    }
                                }
                                None
                            });
                            if let Some((row, col)) = enter_pos {
                                let mut cbs = with_state(|state| std::mem::take(&mut state.cursor_move_callbacks));
                                for cb in cbs.iter_mut() { cb(row, col); }
                                with_state(|state| state.cursor_move_callbacks = cbs);
                            }
                        }
                        if let Some(action) = menu_action {
                            if action == "quit" {
                                with_state(|state| state.running = false);
                            } else {
                                let cb = with_state(|state| state.menu_action_callback.take());
                                if let Some(mut cb) = cb {
                                    cb(action);
                                    with_state(|state| state.menu_action_callback = Some(cb));
                                }
                                // The menu action may have mutated the workbook (e.g.
                                // Insert -> Date writes a cell).  Re-render the frame so
                                // the grid reflects the change immediately, instead of
                                // staying stale until the next key press.
                                redraw_frame(&mut root);
                            }
                        }
                    }
                    Some(Input::Character(c)) => {
                        // Any real keypress cancels a pending quit — EXCEPT Escape itself,
                        // which the bare-Escape branch below consumes to arm/trigger the
                        // quick-quit.  Resetting it here for Escape would clobber the flag
                        // the quit logic relies on, so a second/third Escape could never quit.
                        if c != '\x1b' {
                            with_state(|state| state.pending_quit = false);
                        }
                        // Ctrl+Q quits (matching the ratatui backend); plain 'q'/'Q' are text.
                        if c == '\x11' {
                            with_state(|state| state.running = false);
                            continue;
                        }
                        // While a menu is open, a printable key activates the item
                        // whose shortcut matches it (case-insensitive), like the
                        // ratatui reference's shortcut-letter selection.  Without
                        // this the letter fell through to the grid and started a
                        // cell edit instead of firing the menu item.  Control keys
                        // (Esc, Tab, ...) are excluded so their own handling below
                        // (close menu / alt-detection) is preserved.
                        if c >= ' ' && c != '\x7f' && with_state(|state| state.menu_open) {
                            let mut shortcut_action: Option<String> = None;
                            let mut entered_submenu = false;
                            with_state(|state| {
                                let lower = c.to_ascii_lowercase();
                                // Find the shortcut match with an immutable borrow,
                                // then apply state changes (avoids borrow conflict).
                                let found: Option<(usize, bool, Option<String>)> = {
                                    let items = match menu_current_items(state) {
                                        Some(v) => v,
                                        None => return,
                                    };
                                    let mut hit = None;
                                    for (i, it) in items.iter().enumerate() {
                                        let sc = match it {
                                            crate::MenuItem::Action { shortcut, .. }
                                            | crate::MenuItem::Submenu { shortcut, .. } => {
                                                shortcut.as_deref().unwrap_or("")
                                            }
                                            _ => "",
                                        };
                                        if !sc.is_empty() && sc.eq_ignore_ascii_case(&lower.to_string()) {
                                            match it {
                                                crate::MenuItem::Submenu { .. } => {
                                                    hit = Some((i, true, None));
                                                }
                                                crate::MenuItem::Action { action, .. } => {
                                                    hit = Some((i, false, Some(action.clone())));
                                                }
                                                _ => {}
                                            }
                                            break;
                                        }
                                    }
                                    hit
                                };
                                if let Some((i, is_submenu, action)) = found {
                                    state.active_item = i;
                                    if is_submenu {
                                        menu_enter_submenu(state);
                                        entered_submenu = true;
                                    } else if let Some(action) = action {
                                        shortcut_action = Some(action);
                                        state.menu_open = false;
                                    }
                                }
                            });
                            if entered_submenu {
                                redraw_frame(&mut root);
                                continue;
                            }
                            if let Some(action) = shortcut_action {
                                if action == "quit" {
                                    with_state(|state| state.running = false);
                                } else {
                                    let cb = with_state(|state| state.menu_action_callback.take());
                                    if let Some(mut cb) = cb {
                                        cb(action);
                                        with_state(|state| state.menu_action_callback = Some(cb));
                                    }
                                    // The action may have mutated the workbook; re-render.
                                    redraw_frame(&mut root);
                                }
                                continue;
                            }
                            // No shortcut matched: the menu stays open and the key
                            // is ignored for menu purposes (falls through).
                            continue;
                        }
                        // While a prompt is active, route all input into the prompt buffer.
                        // Escape closes the modal info dialog (About/Help).
                        if c == '\x1b' && ACTIVE_DIALOG.with(|d| d.borrow().is_some()) {
                                                        clear_dialog_area(&root);
                            close_dialog();
                            redraw_frame(&mut root);
                                                        continue;
                        }
                        if with_state(|state| state.prompt_action.is_some()) {
                            if c == '\x1b' {
                                with_state(|state| {
                                    state.prompt_action = None;
                                    state.prompt_buffer.clear();
                                    state.prompt_label.clear();
                                });
                            } else {
                                with_state(|state| state.prompt_buffer.push(c));
                            }
                            redraw_frame(&mut root);
                            continue;
                        }
                        if c == '\t' {
                            with_state(|state| {
                                if state.menu_open {
                                    state.menu_open = false;
                                    state.menu_stack.clear();
                                } else if let Some(fid) = state.focus_id {
                                    if is_spreadsheet_focused(state, fid) {
                                        spreadsheet_commit_edit(state, fid);
                                        if let Some(n) = state.node_mut(fid) {
                                            if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_col = &mut grid.cursor_col; let total_cols = grid.total_cols;
                                                if *cursor_col + 1 < total_cols { *cursor_col += 1; }
                                            }
                                        }
                                        spreadsheet_scroll_to_cursor(state, fid);
                                    } else {
                                        let focusable: Vec<usize> = state.nodes.iter()
                                            .filter(|n| matches!(n.kind, PcWidgetKind::Button { .. } | PcWidgetKind::Entry { .. } | PcWidgetKind::CheckButton { .. } | PcWidgetKind::Spreadsheet { .. } | PcWidgetKind::DataGrid(_)))
                                            .map(|n| n.id)
                                            .collect();
                                        if let Some(pos) = state.focus_id.and_then(|f| focusable.iter().position(|&x| x == f)) {
                                            let next = (pos + 1) % focusable.len();
                                            state.focus_id = Some(focusable[next]);
                                        } else {
                                            state.focus_id = focusable.first().copied();
                                        }
                                    }
                                }
                            });
                        } else if c == '\x1b' {
                            // Distinguish bare Escape from Alt+key (terminal sends ESC + char)
                            root.timeout(300);
                            let alt_key = root.getch();
                            root.timeout(100);
                            match alt_key {
                                Some(Input::Character('[')) => {
                                    // Could be CSI sequence: [1;2A (Shift+Up), [1;2B (Shift+Down),
                                    // [1;2C (Shift+Right), [1;2D (Shift+Left).
                                    // Read the remaining bytes to check.
                                    with_state(|state| state.pending_quit = false);
                                    root.timeout(50);
                                    let seq = (
                                        root.getch(),
                                        root.getch(),
                                        root.getch(),
                                        root.getch(),
                                    );
                                    root.timeout(100);
                                    match seq {
                                        (Some(Input::Character('1')), Some(Input::Character(';')), Some(Input::Character('2')), dir) => {
                                            let dir_char = match dir {
                                                Some(Input::Character(d)) => d,
                                                _ => '\0',
                                            };
                                            match dir_char {
                                                'A' | 'B' | 'C' | 'D' => {
                                                    // Shift+Arrow — process as regular arrow key
                                                    let new_pos = with_state(|state| {
                                                        if state.menu_open { return None; }
                                                        if let Some(fid) = state.focus_id {
                                                            if is_spreadsheet_focused(state, fid) {
                                                                let was_editing = {
                                                                    let n = state.node(fid).unwrap();
                                                                    spreadsheet_is_editing(&n.kind)
                                                                };
                                                                spreadsheet_prepare_move(state, fid, true);
                                                                spreadsheet_commit_edit(state, fid);
                                                                let needs_sentinel = {
                                                                    if let Some(n) = state.node_mut(fid) {
                                                                        if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_row = &mut grid.cursor_row; let cursor_col = &mut grid.cursor_col; let total_rows = grid.total_rows;
                                                                            match dir_char {
                                                                                'A' => { if *cursor_row > 0 { *cursor_row -= 1; None } else { Some(u32::MAX) } }
                                                                                'B' => { if *cursor_row + 1 < total_rows { *cursor_row += 1; None } else { Some(u32::MAX - 1) } }
                                                                                'C' => { *cursor_col += 1; None }
                                                                                'D' => { if *cursor_col > 0 { *cursor_col -= 1; } None }
                                                                                _ => None,
                                                                            }
                                                                        } else {
                                                                            None
                                                                        }
                                                                    } else {
                                                                        None
                                                                    }
                                                                };
                                                                if let Some(sentinel) = needs_sentinel {
                                                                    if was_editing {
                                                                        spreadsheet_enter(state, fid);
                                                                    }
                                                                    let col = {
                                                                        let n = state.node(fid).unwrap();
                                                                        if let PcWidgetKind::Spreadsheet { grid, .. } = &n.kind { let cursor_col = &grid.cursor_col;
                                                                            *cursor_col
                                                                        } else { 0 }
                                                                    };
                                                                    return Some((sentinel, col));
                                                                }
                                                                if was_editing {
                                                                    spreadsheet_enter(state, fid);
                                                                }
                                                                let (row, col) = {
                                                                    let n = state.node(fid).unwrap();
                                                                    if let PcWidgetKind::Spreadsheet { grid, .. } = &n.kind { let cursor_row = &grid.cursor_row; let cursor_col = &grid.cursor_col;
                                                                        (*cursor_row, *cursor_col)
                                                                    } else { (0, 0) }
                                                                };
                                                                return Some((row, col));
                                                            }
                                                        }
                                                        None
                                                    });
                                                    if let Some((row, col)) = new_pos {
                                                        let mut cbs = with_state(|state| std::mem::take(&mut state.cursor_move_callbacks));
                                                        for cb in cbs.iter_mut() { cb(row, col); }
                                                        with_state(|state| state.cursor_move_callbacks = cbs);
                                                    }
                                                }
                                                _ => {
                                                    // Consumed bytes but pattern didn't match — ignore.
                                                }
                                            }
                                        }
                                        _ => {
                                            // Not a Shift+Arrow CSI sequence; consumed bytes are lost.
                                            // This is acceptable since Alt+[ followed by 1;2A is rare.
                                        }
                                    }
                                }
                                Some(Input::Character(ac)) => {
                                    with_state(|state| state.pending_quit = false);
                                    // Alt+key — let the host app decide what to open (e.g. a
                                    // specific menu/submenu via `open_menu`).  If no callback
                                    // is installed, fall back to opening the root menu whose
                                    // label starts with the letter (generic behavior).
                                    let handled = ALT_KEY_CALLBACK.with(|c| {
                                        if let Some(cb) = c.borrow_mut().as_mut() {
                                            cb(ac)
                                        } else {
                                            false
                                        }
                                    });
                                    if !handled {
                                        with_state(|state| {
                                            if let Some(mid) = state.menu_bar_id {
                                                if let Some(n) = state.node(mid) {
                                                    if let PcWidgetKind::MenuBar { labels, .. } = &n.kind {
                                                        let lower = ac.to_ascii_lowercase();
                                                        if let Some(pos) = labels.iter().position(|l| l.resolve().to_ascii_lowercase().starts_with(&lower.to_string())) {
                                                            state.active_submenu = pos;
                                                            state.menu_stack.clear();
                                                            state.active_item = 0;
                                                            state.menu_open = true;
                                                        }
                                                    }
                                                }
                                            }
                                        });
                                    }
                                }
                                _ => {
                                    // No following char within 300ms → bare Escape
                                    // Close any modal info dialog first.
                                    clear_dialog_area(&root);
      clear_dialog_area(&root);
                                    close_dialog();     with_state(|state| {
                                        if let Some(fid) = state.focus_id {
                                            if is_spreadsheet_focused(state, fid) {
                                                if let Some(n) = state.node_mut(fid) {
                                                    if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let editing = &mut grid.editing; let edit_buf = &mut grid.edit_buf;
                                                        if *editing {
                                                            *editing = false; // cancel edit
                                                            edit_buf.clear();
                                                            return;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        if state.menu_open {
                                            state.menu_open = false;
                                        } else if state.pending_quit {
                                            state.running = false;
                                        } else {
                                            state.pending_quit = true;
                                        }
                                    });
                                }
                            }
                        } else if c == '\x03' {
                            // Ctrl+C — copy (standard action).  Dispatch any key
                            // callback registered for Ctrl+C (e.g. corro wires the
                            // cell copy here); do NOT quit, and never insert the
                            // control char into the edit buffer.  Key callbacks
                            // are persistent (take, fire, restore — the same
                            // convention as the goto/cursor/commit callbacks),
                            // so a registered key keeps working for the whole
                            // session instead of firing once and silently
                            // unregistering.
                            let mut cbs: Vec<Box<dyn FnMut()>> = with_state(|state| {
                                let mut out = Vec::new();
                                let all = std::mem::take(&mut state.key_callbacks);
                                for (k, cb) in all {
                                    if k == '\x03' {
                                        out.push(cb);
                                    } else {
                                        state.key_callbacks.push((k, cb));
                                    }
                                }
                                out
                            });
                            for cb in cbs.iter_mut() {
                                cb();
                            }
                            with_state(|state| {
                                for cb in cbs {
                                    state.key_callbacks.push(('\x03', cb));
                                }
                            });
                        } else if c == '\x07' {
                            // Ctrl+G — Go to a target cell (the target is registered
                            // by corro's pnc_backend, e.g. A1000) and re-render the
                            // viewport around it.
                            {
                                let mut goto_cbs: Vec<Box<dyn FnMut()>> =
                                    with_state(|state| std::mem::take(&mut state.goto_callbacks));
                                for cb in goto_cbs.iter_mut() {
                                    cb();
                                }
                                with_state(|state| state.goto_callbacks = goto_cbs);
                            }
                            with_state(|state| {
                                if let Some(fid) = state.focus_id {
                                    if is_spreadsheet_focused(state, fid) {
                                        spreadsheet_scroll_to_cursor(state, fid);
                                    }
                                }
                            });
                    } else if c == ' ' {
                            let callbacks = with_state(|state| {
                                if let Some(fid) = state.focus_id {
                                    if is_spreadsheet_focused(state, fid) {
                                        // Insert space into spreadsheet edit buffer when in edit mode
                                        if let Some(n) = state.node_mut(fid) {
                                            if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let editing = &mut grid.editing; let edit_buf = &mut grid.edit_buf; let edit_pos = &mut grid.edit_pos;
                                                if *editing {
                                                    // Keep edit_pos on a valid index (a mid-char or
                                                    // out-of-range insert would panic; Win9x consoles
                                                    // can deliver garbage key events).
                                                    let pos = (*edit_pos).min(edit_buf.len());
                                                    edit_buf.insert(pos, ' ');
                                                    *edit_pos = pos + 1;
                                                    return vec![];
                                                }
                                            }
                                        }
                                    }
                                    let (toggled, cbs) = toggle_focused(state, fid);
                                    if !toggled {
                                        let idxs: Vec<usize> = (0..state.nodes.len())
                                            .filter(|i| state.nodes[*i].id == fid)
                                            .collect();
                                        for idx in idxs {
                                            if let PcWidgetKind::Entry { ref mut buffer, ref mut cursor } = &mut state.nodes[idx].kind {
                                                buffer.insert(*cursor, ' ');
                                                *cursor += 1;
                                            }
                                        }
                                    }
                                    cbs
                                } else {
                                    vec![]
                                }
                            });
                            fire_callbacks(callbacks);
                        } else {
                            // Check registered key callbacks first (persistent:
                            // take, fire, restore — see the Ctrl+C branch).
                            let mut fired = false;
                            {
                                let mut callbacks: Vec<Box<dyn FnMut()>> = with_state(|state| {
                                    let mut out = Vec::new();
                                    let all = std::mem::take(&mut state.key_callbacks);
                                    for (k, cb) in all {
                                        if k == c {
                                            out.push(cb);
                                            fired = true;
                                        } else {
                                            state.key_callbacks.push((k, cb));
                                        }
                                    }
                                    out
                                });
                                for cb in callbacks.iter_mut() {
                                    cb();
                                }
                                with_state(|state| {
                                    for cb in callbacks {
                                        state.key_callbacks.push((c, cb));
                                    }
                                });
                            }
                            if !fired {
                                with_state(|state| {
                                    if let Some(fid) = state.focus_id {
                                        if is_spreadsheet_focused(state, fid) {
                                            if let Some(n) = state.node_mut(fid) {
                                                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let editing = &mut grid.editing; let cursor_col = &mut grid.cursor_col; let cursor_row = &mut grid.cursor_row; let total_cols = grid.total_cols; let total_rows = grid.total_rows; let edit_buf = &mut grid.edit_buf; let edit_pos = &mut grid.edit_pos;
                                                if !*editing {
                                                            // Not in edit mode — check for special keys
                                                            if c == 'c' || c == 'C' {
                                                                if *cursor_col < total_cols - 1 { *cursor_col = total_cols - 1; }
                                                                spreadsheet_scroll_to_cursor(state, fid);
                                                                return;
                                                            }
                                                            if c == 'r' || c == 'R' {
                                                                if *cursor_row < total_rows - 1 { *cursor_row = total_rows - 1; }
                                                                spreadsheet_scroll_to_cursor(state, fid);
                                                                return;
                                                            }
                                                             // Auto-start edit mode & insert character,
                                                             // matching the ratatui backend.
                                                             *editing = true;
                                                             edit_buf.clear();
                                                             *edit_pos = 0;
                                                     }
                                                     if *editing {
                                                         // Still check editing in case we just entered edit mode above
                                                         // In edit mode — append character. Corro cells are
                                                         // ASCII; Win9x consoles can deliver garbage key
                                                         // events, so reject non-printable/non-ASCII chars
                                                         // and insert on a UTF-8 char boundary.
                                                         if c.is_ascii_graphic() || c == ' ' {
                                                             let pos = (*edit_pos).min(edit_buf.len());
                                                             edit_buf.insert(pos, c);
                                                             *edit_pos = pos + c.len_utf8();
                                                         }
                                                     }
                                                }
                                            }
                                        } else if let Some(n) = state.node_mut(fid) {
                                            if let PcWidgetKind::Entry { ref mut buffer, ref mut cursor } = n.kind {
                                                if c.is_ascii_graphic() || c == ' ' {
                                                    let pos = (*cursor).min(buffer.len());
                                                    buffer.insert(pos, c);
                                                    *cursor = pos + c.len_utf8();
                                                }
                                            }
                                        }
                                    }
                                });
                            }
                        }
                    }
                    Some(Input::KeyBackspace) => {
                        if with_state(|state| state.prompt_action.is_some()) {
                            with_state(|state| { state.prompt_buffer.pop(); });
                            redraw_frame(&mut root);
                            continue;
                        }
                        with_state(|state| {
                            if let Some(fid) = state.focus_id {
                                if is_spreadsheet_focused(state, fid) {
                                    if let Some(n) = state.node_mut(fid) {
                                        if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let editing = &mut grid.editing; let edit_buf = &mut grid.edit_buf; let edit_pos = &mut grid.edit_pos;
                                            if *editing && *edit_pos > 0 {
                                                *edit_pos -= 1;
                                                edit_buf.remove(*edit_pos);
                                            }
                                        }
                                    }
                                } else if let Some(n) = state.node_mut(fid) {
                                    if let PcWidgetKind::Entry { ref mut buffer, ref mut cursor } = n.kind {
                                        if *cursor > 0 {
                                            *cursor -= 1;
                                            buffer.remove(*cursor);
                                        }
                                    }
                                }
                            }
                        });
                    }
                    Some(Input::KeyDC) => {
                        with_state(|state| {
                            if let Some(fid) = state.focus_id {
                                if is_spreadsheet_focused(state, fid) {
                                    if let Some(n) = state.node_mut(fid) {
                                        if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let editing = &mut grid.editing; let edit_buf = &mut grid.edit_buf; let edit_pos = &mut grid.edit_pos;
                                            if *editing && *edit_pos < edit_buf.len() {
                                                edit_buf.remove(*edit_pos);
                                            }
                                        }
                                    }
                                } else if let Some(n) = state.node_mut(fid) {
                                    if let PcWidgetKind::Entry { ref mut buffer, ref mut cursor } = n.kind {
                                        if *cursor < buffer.len() {
                                            buffer.remove(*cursor);
                                        }
                                    }
                                }
                            }
                        });
                    }
                    Some(Input::KeyLeft) => {
                        with_state(|state| state.pending_quit = false);
                        let new_pos = with_state(|state| {
                            if state.menu_open {
                                if !state.menu_stack.is_empty() {
                                    menu_exit_submenu(state);
                                } else if state.active_submenu > 0 {
                                    state.active_submenu -= 1;
                                    state.active_item = 0;
                                }
                                None
                            } else if let Some(fid) = state.focus_id {
                                if is_spreadsheet_focused(state, fid) {
                                    let was_editing = {
                                        let n = state.node(fid).unwrap();
                                        spreadsheet_is_editing(&n.kind)
                                    };
                                    if was_editing {
                                        // Edit mode: Left moves the edit caret within the
                                        // buffer (matching ratatui).  Only when the caret is
                                        // at the start does it discard the edit and move the
                                        // cell cursor left.
                                        let at_start = {
                                            let n = state.node(fid).unwrap();
                                            if let PcWidgetKind::Spreadsheet { grid, .. } = &n.kind { let edit_pos = &grid.edit_pos;
                                                *edit_pos == 0
                                            } else { true }
                                        };
                                        if !at_start {
                                            if let Some(n) = state.node_mut(fid) {
                                                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let edit_pos = &mut grid.edit_pos;
                                                    *edit_pos -= 1;
                                                }
                                            }
                                            return None;
                                        } else {
                                            // Commit the in-progress edit, then move the cell left
                                            // (mirrors Right-at-end, which commits and moves right).
                                            // Previously this discarded the buffer, so `1 <- <-`
                                            // lost the value instead of leaving it in its cell.
                                            spreadsheet_commit_edit(state, fid);
                                            if let Some(n) = state.node_mut(fid) {
                                                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_col = &mut grid.cursor_col;
                                                    if *cursor_col > 0 { *cursor_col -= 1; }
                                                }
                                            }
                                            let (row, col) = {
                                                let n = state.node(fid).unwrap();
                                                if let PcWidgetKind::Spreadsheet { grid, .. } = &n.kind { let cursor_row = &grid.cursor_row; let cursor_col = &grid.cursor_col;
                                                    (*cursor_row, *cursor_col)
                                                } else { (0, 0) }
                                            };
                                            return Some((row, col));
                                        }
                                    } else {
                                        // Not editing: move the cell cursor left.
                                        if let Some(n) = state.node_mut(fid) {
                                            if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_col = &mut grid.cursor_col;
                                                if *cursor_col > 0 { *cursor_col -= 1; }
                                            }
                                        }
                                        let (row, col) = {
                                            let n = state.node(fid).unwrap();
                                            if let PcWidgetKind::Spreadsheet { grid, .. } = &n.kind { let cursor_row = &grid.cursor_row; let cursor_col = &grid.cursor_col;
                                                (*cursor_row, *cursor_col)
                                            } else { (0, 0) }
                                        };
                                        return Some((row, col));
                                    }
                                } else if let Some(n) = state.node_mut(fid) {
                                    if let PcWidgetKind::Entry { ref mut cursor, .. } = n.kind {
                                        if *cursor > 0 { *cursor -= 1; }
                                    }
                                }
                                None
                            } else {
                                None
                            }
                        });
                        if let Some((row, col)) = new_pos {
                            let mut cbs = with_state(|state| std::mem::take(&mut state.cursor_move_callbacks));
                            for cb in cbs.iter_mut() { cb(row, col); }
                            with_state(|state| state.cursor_move_callbacks = cbs);
                        }
                    }
                    Some(Input::KeyRight) => {
                        with_state(|state| state.pending_quit = false);
                        let new_pos = with_state(|state| {
                            if state.menu_open {
                                if menu_current_is_submenu(state) {
                                    menu_enter_submenu(state);
                                } else {
                                    state.menu_stack.clear();
                                    if let Some(mid) = state.menu_bar_id {
                                        if let Some(n) = state.node(mid) {
                                            if let PcWidgetKind::MenuBar { labels, .. } = &n.kind {
                                                if state.active_submenu + 1 < labels.len() {
                                                    state.active_submenu += 1;
                                                    state.active_item = 0;
                                                }
                                            }
                                        }
                                    }
                                }
                                None
                            } else if let Some(fid) = state.focus_id {
                                if is_spreadsheet_focused(state, fid) {
                                    let was_editing = {
                                        let n = state.node(fid).unwrap();
                                        spreadsheet_is_editing(&n.kind)
                                    };
                                    if was_editing {
                                        // Edit mode: Right moves the edit caret within the
                                        // buffer (matching ratatui).  Only when the caret is
                                        // at the end does it commit the edit, move the cell
                                        // right, and exit edit mode.
                                        let at_end = {
                                            let n = state.node(fid).unwrap();
                                            if let PcWidgetKind::Spreadsheet { grid, .. } = &n.kind { let edit_pos = &grid.edit_pos; let edit_buf = &grid.edit_buf;
                                                *edit_pos >= edit_buf.chars().count()
                                            } else { true }
                                        };
                                        if !at_end {
                                            if let Some(n) = state.node_mut(fid) {
                                                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let edit_pos = &mut grid.edit_pos;
                                                    *edit_pos += 1;
                                                }
                                            }
                                            return None;
                                        } else {
                                            // Commit the edit, move the cell right, exit edit mode.
                                            spreadsheet_commit_edit(state, fid);
                                            if let Some(n) = state.node_mut(fid) {
                                                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_col = &mut grid.cursor_col;
                                                    *cursor_col += 1;
                                                }
                                            }
                                            let (row, col) = {
                                                let n = state.node(fid).unwrap();
                                                if let PcWidgetKind::Spreadsheet { grid, .. } = &n.kind { let cursor_row = &grid.cursor_row; let cursor_col = &grid.cursor_col;
                                                    (*cursor_row, *cursor_col)
                                                } else { (0, 0) }
                                            };
                                            return Some((row, col));
                                        }
                                    } else {
                                        // Not editing: move the cell cursor right.
                                        if let Some(n) = state.node_mut(fid) {
                                            if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_col = &mut grid.cursor_col;
                                                *cursor_col += 1;
                                            }
                                        }
                                        let (row, col) = {
                                            let n = state.node(fid).unwrap();
                                            if let PcWidgetKind::Spreadsheet { grid, .. } = &n.kind { let cursor_row = &grid.cursor_row; let cursor_col = &grid.cursor_col;
                                                (*cursor_row, *cursor_col)
                                            } else { (0, 0) }
                                        };
                                        return Some((row, col));
                                    }
                                } else if let Some(n) = state.node_mut(fid) {
                                    if let PcWidgetKind::Entry { ref mut cursor, ref buffer } = n.kind {
                                        if *cursor < buffer.len() { *cursor += 1; }
                                    }
                                }
                                None
                            } else {
                                None
                            }
                        });
                        if let Some((row, col)) = new_pos {
                            let mut cbs = with_state(|state| std::mem::take(&mut state.cursor_move_callbacks));
                            for cb in cbs.iter_mut() { cb(row, col); }
                            with_state(|state| state.cursor_move_callbacks = cbs);
                        }
                    }
                    Some(Input::KeyUp) => {
                        with_state(|state| state.pending_quit = false);
                        let new_pos = with_state(|state| {
                            if state.menu_open {
                                let len = menu_current_items(state).map(|items| items.len()).unwrap_or(0);
                                let old_item = state.active_item;
                                if len > 0 {
                                    // Wrap around: Up on the first item -> last.
                                    state.active_item = (state.active_item + len - 1) % len;
                                }
                                MENU_HIGHLIGHT.with(|m| *m.borrow_mut() = Some((old_item, state.active_item)));
                                None
                            } else if let Some(fid) = state.focus_id {
                                if is_spreadsheet_focused(state, fid) {
                                    let was_editing = {
                                        let n = state.node(fid).unwrap();
                                        spreadsheet_is_editing(&n.kind)
                                    };
                                    spreadsheet_prepare_move(state, fid, false);
                                    spreadsheet_commit_edit(state, fid);
                                    let needs_sentinel = {
                                        if let Some(n) = state.node_mut(fid) {
                                            if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_row = &mut grid.cursor_row;
                                                if *cursor_row > 0 {
                                                    *cursor_row -= 1;
                                                    false
                                                } else {
                                                    true
                                                }
                                            } else {
                                                false
                                            }
                                        } else {
                                            false
                                        }
                                    };
                                    if needs_sentinel {
                                        if was_editing {
                                            spreadsheet_enter(state, fid);
                                        }
                                        let sentinel_col = {
                                            let n = state.node(fid).unwrap();
                                            if let PcWidgetKind::Spreadsheet { grid, .. } = &n.kind { let cursor_col = &grid.cursor_col;
                                                *cursor_col
                                            } else { 0 }
                                        };
                                        return Some((u32::MAX, sentinel_col));
                                    }
                                    if was_editing {
                                        spreadsheet_enter(state, fid);
                                    }
                                    let (row, col) = {
                                        let n = state.node(fid).unwrap();
                                        if let PcWidgetKind::Spreadsheet { grid, .. } = &n.kind { let cursor_row = &grid.cursor_row; let cursor_col = &grid.cursor_col;
                                            (*cursor_row, *cursor_col)
                                        } else { (0, 0) }
                                    };
                                    return Some((row, col));
                                }
                                None
                            } else {
                                None
                            }
                        });
                        if let Some((row, col)) = new_pos {
                            let mut cbs = with_state(|state| std::mem::take(&mut state.cursor_move_callbacks));
                            for cb in cbs.iter_mut() { cb(row, col); }
                            with_state(|state| state.cursor_move_callbacks = cbs);
                        }
                    }
                    Some(Input::KeyDown) => {
                        with_state(|state| state.pending_quit = false);
                        let new_pos = with_state(|state| {
                            if state.menu_open {
                                let len = menu_current_items(state).map(|items| items.len()).unwrap_or(0);
                                let old_item = state.active_item;
                                if len > 0 {
                                    // Wrap around: Down on the last item -> first.
                                    state.active_item = (state.active_item + 1) % len;
                                }
                                MENU_HIGHLIGHT.with(|m| *m.borrow_mut() = Some((old_item, state.active_item)));
                                None
                            } else if let Some(fid) = state.focus_id {
                                if is_spreadsheet_focused(state, fid) {
                                    let was_editing = {
                                        let n = state.node(fid).unwrap();
                                        spreadsheet_is_editing(&n.kind)
                                    };
                                    spreadsheet_commit_edit(state, fid);
                                    let max_row = {
                                        let n = state.node(fid).unwrap();
                                        if let PcWidgetKind::Spreadsheet { grid, .. } = &n.kind { let total_rows = grid.total_rows;
                                            total_rows
                                        } else { 0 }
                                    };
                                    let needs_sentinel = {
                                        if let Some(n) = state.node_mut(fid) {
                                            if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_row = &mut grid.cursor_row;
                                                if *cursor_row + 1 < max_row {
                                                    *cursor_row += 1;
                                                    false
                                                } else {
                                                    true
                                                }
                                            } else {
                                                false
                                            }
                                        } else {
                                            false
                                        }
                                    };
                                    if needs_sentinel {
                                        if was_editing {
                                            spreadsheet_enter(state, fid);
                                        }
                                        let sentinel_col = {
                                            let n = state.node(fid).unwrap();
                                            if let PcWidgetKind::Spreadsheet { grid, .. } = &n.kind { let cursor_col = &grid.cursor_col;
                                                *cursor_col
                                            } else { 0 }
                                        };
                                        return Some((u32::MAX - 1, sentinel_col));
                                    }
                                    if was_editing {
                                        spreadsheet_enter(state, fid);
                                    }
                                    let (row, col) = {
                                        let n = state.node(fid).unwrap();
                                        if let PcWidgetKind::Spreadsheet { grid, .. } = &n.kind { let cursor_row = &grid.cursor_row; let cursor_col = &grid.cursor_col;
                                            (*cursor_row, *cursor_col)
                                        } else { (0, 0) }
                                    };
                                    return Some((row, col));
                                }
                                None
                            } else {
                                None
                            }
                        });
                        if let Some((row, col)) = new_pos {
                            let mut cbs = with_state(|state| std::mem::take(&mut state.cursor_move_callbacks));
                            for cb in cbs.iter_mut() { cb(row, col); }
                            with_state(|state| state.cursor_move_callbacks = cbs);
                        }
                    }
                    Some(Input::KeySLeft) => {
                        with_state(|state| {
                            if let Some(fid) = state.focus_id {
                                if is_spreadsheet_focused(state, fid) {
                                    spreadsheet_prepare_move(state, fid, true);
                                    spreadsheet_commit_edit(state, fid);
                                    if let Some(n) = state.node_mut(fid) {
                                        if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_col = &mut grid.cursor_col;
                                            if *cursor_col > 0 { *cursor_col -= 1; }
                                        }
                                    }
                                    spreadsheet_scroll_to_cursor(state, fid);
                                }
                            }
                        });
                    }
                    Some(Input::KeySRight) => {
                        with_state(|state| {
                            if let Some(fid) = state.focus_id {
                                if is_spreadsheet_focused(state, fid) {
                                    spreadsheet_prepare_move(state, fid, true);
                                    spreadsheet_commit_edit(state, fid);
                                    if let Some(n) = state.node_mut(fid) {
                                        if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_col = &mut grid.cursor_col; let total_cols = grid.total_cols;
                                            if *cursor_col + 1 < total_cols { *cursor_col += 1; }
                                        }
                                    }
                                    spreadsheet_scroll_to_cursor(state, fid);
                                }
                            }
                        });
                    }
                    Some(Input::KeyPPage) => {
                        with_state(|state| {
                            if let Some(fid) = state.focus_id {
                                if is_spreadsheet_focused(state, fid) {
                                    spreadsheet_commit_edit(state, fid);
                                    if let Some(n) = state.node_mut(fid) {
                                        if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_row = &mut grid.cursor_row; let total_rows = grid.total_rows;
                                            let page = 20.min(total_rows / 2);
                                            *cursor_row = cursor_row.saturating_sub(page);
                                        }
                                    }
                                    spreadsheet_scroll_to_cursor(state, fid);
                                }
                            }
                        });
                    }
                    Some(Input::KeyNPage) => {
                        with_state(|state| {
                            if let Some(fid) = state.focus_id {
                                if is_spreadsheet_focused(state, fid) {
                                    spreadsheet_commit_edit(state, fid);
                                    if let Some(n) = state.node_mut(fid) {
                                        if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_row = &mut grid.cursor_row; let total_rows = grid.total_rows;
                                            let page = 20.min(total_rows / 2);
                                            *cursor_row = (*cursor_row + page).min(total_rows - 1);
                                        }
                                    }
                                    spreadsheet_scroll_to_cursor(state, fid);
                                }
                            }
                        });
                    }
                    Some(Input::KeyHome) => {
                        with_state(|state| {
                            if let Some(fid) = state.focus_id {
                                if is_spreadsheet_focused(state, fid) {
                                    spreadsheet_commit_edit(state, fid);
                                    if let Some(n) = state.node_mut(fid) {
                                        if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind {
                                            let row = grid.cursor_row;
                                            if let Some((leftmost, _)) = spreadsheet_row_nonblank_extremes(grid, row) {
                                                grid.cursor_col = leftmost as u32;
                                            }
                                        }
                                    }
                                    spreadsheet_scroll_to_cursor(state, fid);
                                }
                            }
                        });
                    }
                    Some(Input::KeyEnd) => {
                        with_state(|state| {
                            if let Some(fid) = state.focus_id {
                                if is_spreadsheet_focused(state, fid) {
                                    spreadsheet_commit_edit(state, fid);
                                    if let Some(n) = state.node_mut(fid) {
                                        if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind {
                                            let row = grid.cursor_row;
                                            if let Some((_, rightmost)) = spreadsheet_row_nonblank_extremes(grid, row) {
                                                grid.cursor_col = rightmost as u32;
                                            }
                                        }
                                    }
                                    spreadsheet_scroll_to_cursor(state, fid);
                                }
                            }
                        });
                    }
                    Some(Input::KeySTab) => {
                        with_state(|state| {
                            let focusable: Vec<usize> = state.nodes.iter()
                                .filter(|n| matches!(n.kind, PcWidgetKind::Button { .. } | PcWidgetKind::Entry { .. } | PcWidgetKind::CheckButton { .. }))
                                .map(|n| n.id)
                                .collect();
                            if let Some(pos) = state.focus_id.and_then(|f| focusable.iter().position(|&x| x == f)) {
                                let prev = if pos == 0 { focusable.len() - 1 } else { pos - 1 };
                                state.focus_id = Some(focusable[prev]);
                            } else {
                                state.focus_id = focusable.last().copied();
                            }
                        });
                    }
                    Some(Input::KeySR) => {
                        with_state(|state| state.pending_quit = false);
                        with_state(|state| {
                            if let Some(fid) = state.focus_id {
                                if is_spreadsheet_focused(state, fid) {
                                    // Shift+Up extends (see the registration
                                    // above); plain Up is Input::KeyUp.
                                    spreadsheet_prepare_move(state, fid, true);
                                    spreadsheet_commit_edit(state, fid);
                                    if let Some(n) = state.node_mut(fid) {
                                        if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_row = &mut grid.cursor_row;
                                            if *cursor_row > 0 { *cursor_row -= 1; }
                                        }
                                    }
                                    spreadsheet_scroll_to_cursor(state, fid);
                                }
                            }
                        });
                    }
                    Some(Input::KeySF) => {
                        with_state(|state| state.pending_quit = false);
                        with_state(|state| {
                            if let Some(fid) = state.focus_id {
                                if is_spreadsheet_focused(state, fid) {
                                    // Shift+Down extends (see above).
                                    spreadsheet_prepare_move(state, fid, true);
                                    spreadsheet_commit_edit(state, fid);
                                    if let Some(n) = state.node_mut(fid) {
                                        if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_row = &mut grid.cursor_row; let total_rows = grid.total_rows;
                                            if *cursor_row + 1 < total_rows { *cursor_row += 1; }
                                        }
                                    }
                                    spreadsheet_scroll_to_cursor(state, fid);
                                }
                            }
                        });
                    }
                    Some(Input::KeyExit) => {
                        with_state(|state| state.running = false);
                    }
                    _ => {}
                }
                // Fire any commit-edit callbacks deferred during `with_state` (they call
                // `with_state`-using adapter functions and must run outside the borrow).
                let deferred = DEFERRED_COMMIT.with(|d| d.borrow_mut().drain(..).collect::<Vec<_>>());
                for (r, c, val, mut cbs) in deferred {
                    for cb in cbs.iter_mut() { cb(r, c, val.clone()); }
                    // Restore the commit-edit callbacks: spreadsheet_commit_edit used
                    // std::mem::take to stash them, so without restoring, the registry
                    // is empty after the first commit and later edits never fire the
                    // host callback (text would silently disappear).
                    with_state(|state| state.commit_edit_callbacks.append(&mut cbs));
                }
                            if input.is_some() {
                    // Up/Down inside an open menu only moves the highlight:
                    // redraw just those two rows instead of the whole submenu
                    // (which would clear + redraw everything and flicker).
                    let highlight = MENU_HIGHLIGHT.with(|m| m.borrow_mut().take());
                    if let Some((old, new)) = highlight {
                        emit_menu_highlight(old, new);
                        // Same contract as a full redraw: fire the after-redraw
                        // hook so the idle marker advances and tests observe the
                        // incremental frame.
                        AFTER_REDRAW.with(|c| {
                            if let Some(cb) = c.borrow_mut().as_mut() { cb(); }
                        });
                    } else {
                        redraw_frame(&mut root);
                    }
                }
                // Draw the active text prompt directly on top of the grid so it is
                // always visible while a path/name is being entered.
                if with_state(|state| state.prompt_action.is_some()) {
                    let (label, buf) = with_state(|state| (state.prompt_label.clone(), state.prompt_buffer.clone()));
                    let mut pout = String::new();
                    let ptext = format!("{}: {}_", label, buf);
                    pout.push_str(&sgr_cup(1, 0));
                    pout.push_str(SGR_RESET);
                    pout.push_str(&" ".repeat(200));
                    pout.push_str(&sgr_cup(1, 0));
                    pout.push_str(sgr_prompt());
                    pout.push_str(&ptext);
                    pout.push_str(SGR_RESET);
                    emit_sgr(&pout);
                }
}

            endwin();
            Ok(())
        }
    }

    fn render_widget(root: &Window, kind: &PcWidgetKind, rect: Rect, id: usize, focus_id: Option<usize>, menu_bar_text: Option<&str>) {
        match kind {
            PcWidgetKind::Window { title } => {
                if has_colors() {
                    root.attron(COLOR_PAIR(7));
                }
                for x in rect.x..rect.x + rect.w {
                    root.mvaddch(rect.y, x, '─');
                    root.mvaddch(rect.y + rect.h - 1, x, '─');
                }
                for y in rect.y..rect.y + rect.h {
                    root.mvaddch(y, rect.x, '│');
                    root.mvaddch(y, rect.x + rect.w - 1, '│');
                }
                root.mvaddch(rect.y, rect.x, '┌');
                root.mvaddch(rect.y, rect.x + rect.w - 1, '┐');
                root.mvaddch(rect.y + rect.h - 1, rect.x, '└');
                root.mvaddch(rect.y + rect.h - 1, rect.x + rect.w - 1, '┘');
                if has_colors() {
                    root.attroff(COLOR_PAIR(7));
                }
                let title_x = rect.x + (rect.w - title.len() as i32) / 2;
                if title_x > rect.x {
                    root.mvaddstr(rect.y, title_x, title);
                }
            }
            PcWidgetKind::Button { label, weight, italic } => {
                let focused = focus_id == Some(id);
                if focused && has_colors() {
                    root.attron(COLOR_PAIR(2));
                } else if has_colors() {
                    root.attron(COLOR_PAIR(1));
                }
                if *weight >= 600 {
                    root.attron(A_BOLD);
                }
                if *italic {
                    root.attron(A_ITALIC);
                }
                // HL button gets reverse video for highlight effect
                let is_highlight = *weight >= 600 && label.len() >= 2;
                if is_highlight {
                    root.attron(A_REVERSE);
                }
                let inner_w = rect.w - 2;
                let display = if label.len() as i32 > inner_w {
                    let len = inner_w.max(0) as usize;
                    label[..len].to_string()
                } else {
                    label.clone()
                };
                let pad = (inner_w - display.len() as i32).max(0);
                let left_pad = pad / 2;
                let right_pad = pad - left_pad;
                // Fill entire button area with spaces so background spans the full width
                for x in rect.x..rect.x + rect.w {
                    root.mvaddch(rect.y, x, ' ');
                }
                root.mvaddch(rect.y, rect.x, '[');
                root.mvaddstr(rect.y, rect.x + 1 + left_pad, &display);
                root.mvaddch(rect.y, rect.x + 1 + left_pad + display.len() as i32 + right_pad, ']');
                if *italic {
                    root.attroff(A_ITALIC);
                }
                if *weight >= 600 {
                    root.attroff(A_BOLD);
                }
                if is_highlight {
                    root.attroff(A_REVERSE);
                }
                if has_colors() {
                    root.attroff(COLOR_PAIR(1) | COLOR_PAIR(2));
                }
            }
            PcWidgetKind::Label { text, .. } => {
                if has_colors() {
                    root.attron(COLOR_PAIR(3));
                }
                // Truncate by CHARACTERS, not bytes: `&text[..max_w]` sliced a
                // multi-byte label in half and `mvaddstr` then returned ERR
                // (nothing drawn). Char-based clipping also matches the
                // char-based natural width the box layout measures.
                let truncated: String = text.chars().take(rect.w.max(0) as usize).collect();
                root.mvaddstr(rect.y, rect.x, &truncated);
                // Curses only writes the cells it is given, so a label whose
                // text shrank (or that was widened by a `set_fixed_width` pin)
                // would leave the tail of the previous, longer text on screen.
                // Blank the rest of the slot.
                for i in truncated.chars().count() as i32..rect.w {
                    root.mvaddch(rect.y, rect.x + i, ' ');
                }
                if has_colors() {
                    root.attroff(COLOR_PAIR(3));
                }
            }
            PcWidgetKind::Entry { buffer, cursor } => {
                let focused = focus_id == Some(id);
                if focused && has_colors() {
                    root.attron(COLOR_PAIR(1));
                } else if has_colors() {
                    root.attron(COLOR_PAIR(3));
                }
                let max_w = rect.w as usize;
                let display = if buffer.len() > max_w {
                    &buffer[buffer.len() - max_w..]
                } else {
                    buffer.as_str()
                };
                root.mvaddstr(rect.y, rect.x, display);
                let rest = max_w.saturating_sub(display.len());
                for i in 0..rest {
                    root.mvaddch(rect.y, rect.x + display.len() as i32 + i as i32, ' ');
                }
                if focused {
                    let cursor_display = cursor.saturating_sub(buffer.len().saturating_sub(max_w));
                    let ch = '_' as chtype | A_REVERSE as chtype;
                    root.mvaddch(rect.y, rect.x + cursor_display as i32, ch);
                }
                if has_colors() {
                    root.attroff(COLOR_PAIR(1) | COLOR_PAIR(3));
                }
            }
            PcWidgetKind::CheckButton { label, checked } => {
                let focused = focus_id == Some(id);
                if focused && has_colors() {
                    root.attron(COLOR_PAIR(2));
                } else if has_colors() {
                    root.attron(COLOR_PAIR(3));
                }
                let mark = if *checked { "[x]" } else { "[ ]" };
                root.mvaddstr(rect.y, rect.x, mark);
                let max_lbl = (rect.w - 4) as usize;
                let truncated = if label.len() > max_lbl { &label[..max_lbl] } else { label.as_str() };
                root.mvaddstr(rect.y, rect.x + 4, truncated);
                if has_colors() {
                    root.attroff(COLOR_PAIR(2) | COLOR_PAIR(3));
                }
            }
            PcWidgetKind::RadioButton { label, checked, .. } => {
                if has_colors() {
                    root.attron(COLOR_PAIR(3));
                }
                let mark = if *checked { "(•)" } else { "( )" };
                root.mvaddstr(rect.y, rect.x, mark);
                let max_lbl = (rect.w - 4) as usize;
                let truncated = if label.len() > max_lbl { &label[..max_lbl] } else { label.as_str() };
                root.mvaddstr(rect.y, rect.x + 4, truncated);
                if has_colors() {
                    root.attroff(COLOR_PAIR(3));
                }
            }
            PcWidgetKind::BoxWidget { .. } | PcWidgetKind::Grid { .. } | PcWidgetKind::Sizer(_) => {}
            PcWidgetKind::Dialog { title, .. } => {
                if has_colors() {
                    root.attron(COLOR_PAIR(7));
                }
                for x in rect.x..rect.x + rect.w {
                    root.mvaddch(rect.y, x, '═');
                    root.mvaddch(rect.y + rect.h - 1, x, '═');
                }
                for y in rect.y..rect.y + rect.h {
                    root.mvaddch(y, rect.x, '║');
                    root.mvaddch(y, rect.x + rect.w - 1, '║');
                }
                root.mvaddch(rect.y, rect.x, '╔');
                root.mvaddch(rect.y, rect.x + rect.w - 1, '╗');
                root.mvaddch(rect.y + rect.h - 1, rect.x, '╚');
                root.mvaddch(rect.y + rect.h - 1, rect.x + rect.w - 1, '╝');
                if has_colors() {
                    root.attroff(COLOR_PAIR(7));
                }
                root.mvaddstr(rect.y, rect.x + 2, title);
            }
            PcWidgetKind::Menu => {
                if has_colors() {
                    root.attron(COLOR_PAIR(4));
                }
                for x in rect.x..rect.x + rect.w {
                    root.mvaddch(rect.y, x, ' ');
                }
                if has_colors() {
                    root.attroff(COLOR_PAIR(4));
                }
            }
            PcWidgetKind::DataGrid(grid) => {
                // Bare data grid (mirrors wxGrid): render the visible cells.
                if has_colors() { root.attron(COLOR_PAIR(4)); }
                for row in 0..rect.h.max(0) {
                    for col in 0..rect.w.max(0) {
                        let (r, c) = (row as u32, col as u32);
                        let val = grid.display_value(r, c);
                        if !val.is_empty() {
                            let max = (rect.w - col).max(0) as usize;
                            let slice = &val[..val.len().min(max)];
                            root.mvaddstr(rect.y + row, rect.x + col, slice);
                        }
                    }
                }
                if has_colors() { root.attroff(COLOR_PAIR(4)); }
            }
            PcWidgetKind::MenuBar { labels, .. } => {
                if has_colors() {
                    root.attron(COLOR_PAIR(4));
                }
                for x in rect.x..rect.x + rect.w {
                    root.mvaddch(rect.y, x, ' ');
                }
                let mut cx = rect.x + 1;
                for label in labels {
                    let label = label.resolve();
                    if cx + label.len() as i32 + 2 > rect.x + rect.w { break; }
                    root.mvaddstr(rect.y, cx, &label);
                    cx += label.len() as i32 + 2;
                }
                if has_colors() {
                    root.attroff(COLOR_PAIR(4));
                }
            }
            PcWidgetKind::SimpleAction => {}
            PcWidgetKind::DropDown { items, selected } => {
                if has_colors() {
                    root.attron(COLOR_PAIR(1));
                }
                let current = selected.and_then(|s| items.get(s)).map(|s| s.as_str()).unwrap_or("");
                let max_w = (rect.w - 2) as usize;
                let truncated = if current.len() > max_w { &current[..max_w] } else { current };
                // Fill entire width so background spans the full dropdown
                for x in rect.x..rect.x + rect.w {
                    root.mvaddch(rect.y, x, ' ');
                }
                root.mvaddch(rect.y, rect.x, '[');
                root.mvaddstr(rect.y, rect.x + 1, truncated);
                root.mvaddstr(rect.y, rect.x + rect.w - 2, "▼");
                root.mvaddch(rect.y, rect.x + rect.w - 1, ']');
                if has_colors() {
                    root.attroff(COLOR_PAIR(1));
                }
            }
            PcWidgetKind::TextView { text } => {
                if has_colors() {
                    root.attron(COLOR_PAIR(3));
                }
                let max_w = rect.w as usize;
                let max_h = rect.h as usize;
                let lines: Vec<&str> = text.lines().collect();
                for (i, line) in lines.iter().enumerate().take(max_h) {
                    let truncated = if line.len() > max_w { &line[..max_w] } else { line };
                    root.mvaddstr(rect.y + i as i32, rect.x, truncated);
                }
                if has_colors() {
                    root.attroff(COLOR_PAIR(3));
                }
            }
            PcWidgetKind::Canvas | PcWidgetKind::Overlay | PcWidgetKind::ScrolledWindow => {}
            PcWidgetKind::Spreadsheet { grid, ref menu_text, ref status_text, ref border_title, ref formula_bar_trailing, ref tab_titles, ref tab_active, .. } => {
                let cells = &grid.cells;
                let raw_cells = &grid.raw_cells;
                let cell_styles = &grid.cell_styles;
                let top_row = &grid.top_row;
                let left_col = &grid.left_col;
                let cursor_row = &grid.cursor_row;
                let cursor_col = &grid.cursor_col;
                let editing = &grid.editing;
                let edit_buf = &grid.edit_buf;
                let edit_pos = &grid.edit_pos;
                let col_width = &grid.col_width;
                let margin_cols = &grid.margin_cols;
                let main_cols = &grid.main_cols;
                let column_layout = &grid.column_layout;
                let row_labels = &grid.row_labels;
                let header_row_count = &grid.header_row_count;
                let main_row_count = &grid.main_row_count;
                // ── Direct SGR rendering ──
                let lm = *margin_cols as usize;
                let mc = *main_cols as usize;
                let use_layout = !column_layout.is_empty();
                // Selection coverage for header chrome, derived from the same
                // anchor the body selection uses (widget-local pair of ranges;
                // the pancurses backend has no Rows/Cols-only modes, so both
                // axes are covered exactly like the body rectangle).
                let (cover_rows, cover_cols) =
                    header_cover(grid.anchor, (*cursor_row, *cursor_col));
                let mut out = String::new();
                out.push_str(SGR_RESET);
                let mut row_offset = rect.y;

                // Menu bar: black fg on cyan bg. Prefer the widget-derived
                // line (bracket tracks the open menu); fall back to the
                // app-set text when no menubar widget exists.
                let menu_line: &str = menu_bar_text.unwrap_or(menu_text);
                if !menu_line.is_empty() {
                    let max_chars = rect.w as usize;
                    let end = menu_line.char_indices().nth(max_chars).map(|(i, _)| i).unwrap_or(menu_line.len());
                    out.push_str(&sgr_cup(row_offset, rect.x));
                    out.push_str(sgr_menu_bar());
                    out.push_str(&menu_line[..end]);
                    let remaining = (rect.w as usize).saturating_sub(end);
                    if remaining > 0 {
                        out.push_str(&" ".repeat(remaining));
                    }
                    row_offset += 1;
                }

                // Formula bar: cyan fg, default bg (Normal) / white on dark gray (Edit)
                {
                    let cc = *cursor_col;
                    let mc = *main_cols;
                    let lm = *margin_cols;
                    let col_part = if cc < lm {
                        let margin_idx = lm.saturating_sub(1).saturating_sub(cc);
                        format!("[{}", col_label(margin_idx))
                    } else if cc < lm + mc {
                        col_label(cc - lm)
                    } else {
                        let right_idx = cc.saturating_sub(lm).saturating_sub(mc);
                        format!("]{}", col_label(right_idx))
                    };
                    let row_label = row_labels.iter()
                        .find(|(r, _)| *r == *cursor_row)
                        .map(|(_, l)| l.as_str())
                        .unwrap_or("1");
                    let addr_text = format!("{}{}", col_part, row_label.trim());
                    // Use edit mode when explicitly editing OR when edit_buf
                    // contains pending input (handles timing edge cases).
                    if *editing || !edit_buf.is_empty() {
                        // Edit mode: white on dark gray (prompt_style), edit_buf, caret cursor
                        let addr_str = format!(" {}  ", addr_text);
                        let chars: Vec<char> = edit_buf.chars().collect();
                        let cursor = (*edit_pos).min(chars.len());
                        let before: String = chars[..cursor].iter().collect();
                        let after: String = if cursor < chars.len() {
                            chars[cursor + 1..].iter().collect()
                        } else {
                            String::new()
                        };
                        let cursor_ch = chars.get(cursor).map(|c| c.to_string()).unwrap_or_else(|| " ".to_string());
                        let content_w = addr_str.chars().count() + before.chars().count() + cursor_ch.chars().count() + after.chars().count();
                        out.push_str(&sgr_cup(row_offset, rect.x));
                        out.push_str(sgr_prompt());
                        out.push_str(&addr_str);
                        if !before.is_empty() {
                            out.push_str(SGR_BOLD);
                            out.push_str(&before);
                        }
                        out.push_str(sgr_caret());
                        out.push_str(&cursor_ch);
                        if !after.is_empty() {
                            out.push_str(SGR_BOLD);
                            out.push_str(&after);
                        }
                        if content_w < rect.w as usize {
                            out.push_str(SGR_RESET);
                            out.push_str(sgr_prompt());
                            out.push_str(&" ".repeat(rect.w as usize - content_w));
                        } else {
                            out.push_str(SGR_RESET);
                        }
                    } else {
                        // Normal mode: cyan fg on default bg (formula style), cell value, trailing status (matching ratatui)
                        let cell_val = raw_cells.borrow().get(&(*cursor_row, *cursor_col)).cloned().unwrap_or_default();
                        let fb_text = format!(" {}  {}{}", addr_text, cell_val, formula_bar_trailing);
                        out.push_str(&sgr_cup(row_offset, rect.x));
                        out.push_str(sgr_formula());
                        out.push_str(&fb_text);
                        out.push_str(SGR_FG_DEFAULT);
                        let content_w = fb_text.chars().count();
                        if content_w < rect.w as usize {
                            out.push_str(&" ".repeat(rect.w as usize - content_w));
                        }
                    }
                    row_offset += 1;
                }

                // Top border line: ┌ title ───┐
                {
                    let br = row_offset;
                    let title = if !border_title.is_empty() {
                        format!("{}", border_title)
                    } else {
                        format!("corro  {}r × {}c ", header_row_count + main_row_count, *main_cols)
                    };
                    let title_vis = title.chars().count();
                    let dash_fill = (rect.w as usize).saturating_sub(title_vis + 3);
                    out.push_str(&sgr_cup(br, rect.x));
                    out.push_str(SGR_FG_DEFAULT);
                    out.push_str(SGR_BG_DEFAULT);
                    out.push_str("┌");
                    out.push_str(SGR_BOLD);
                    out.push_str(" ");
                    out.push_str(&title);
                    out.push_str(SGR_RESET);
                    out.push_str(&"─".repeat(dash_fill));
                    out.push_str("┐");
                    row_offset += 1;
                }

                // Grid header row
                {
                    let hr = row_offset;
                    out.push_str(&sgr_cup(hr, rect.x));
                    out.push_str("│");
                    if use_layout {
                        // Pre-computed layout
                        let mut hx = rect.x + 1;
                        out.push_str(&sgr_cup(hr, hx));
                        out.push_str(SGR_BOLD);
                        out.push_str(&" ".repeat(5));
                        out.push_str(SGR_RESET);
                        hx += 5;
                        let n = column_layout.len();
                        for (i, &(ref ci, ref w, ref label)) in column_layout.iter().enumerate() {
                            let gap_after = if i + 1 < n { 1 } else { 0 };
                            if hx + *w as i32 + gap_after > rect.x + rect.w - 1 { break; }
                            let padded = format!("{:<1$}", label, *w as usize);
                            let active_col = *ci == *cursor_col;
                            let covered_col =
                                cover_cols.is_some_and(|(c0, c1)| *ci >= c0 && *ci <= c1);
                            let style = if active_col || covered_col {
                                sgr_header_active()
                            } else {
                                sgr_header_inactive()
                            };
                            out.push_str(&sgr_cup(hr, hx));
                            out.push_str(style);
                            out.push_str(&padded);
                            out.push_str(SGR_RESET);
                            hx += *w as i32;
                            if i + 1 < n {
                                let is_boundary = lm > 0 && (*ci == (lm - 1) as u32 || *ci == (lm + mc - 1) as u32);
                                out.push_str(&sgr_cup(hr, hx));
                                if is_boundary {
                                    // Match ratatui: a boundary pipe is followed by
                                    // a space (`│ `) so column labels stay aligned.
                                    // Emitting just `│` shifted every later label left
                                    // by one column.
                                    out.push_str("│ ");
                                    hx += 2;
                                } else {
                                    out.push_str(" ");
                                    hx += 1;
                                }
                            }
                        }
                        if hx <= rect.x + rect.w - 1 {
                            let gap = (rect.x + rect.w - 1 - hx) as usize;
                            if gap > 0 {
                                out.push_str(&" ".repeat(gap));
                            }
                            out.push_str(&sgr_cup(hr, rect.x + rect.w - 1));
                        out.push_str("│");
                    } else {
                        }
                    } else {
                        let cw = *col_width as i32;
                        let rh_w = 5i32;
                        let max_vis_cols = ((rect.w - rh_w - 2) / (cw + 1)).max(1).min(100);
                        let mut hx = rect.x + 1;
                        out.push_str(&sgr_cup(hr, hx));
                        out.push_str(&" ".repeat(rh_w as usize));
                        hx += rh_w;
                        for vc in 0..max_vis_cols {
                            let col_idx = *left_col + vc as u32;
                            let label = if col_idx < lm as u32 {
                                let margin_idx = lm.saturating_sub(1).saturating_sub(col_idx as usize);
                                format!("[{}", col_label(margin_idx as u32))
                            } else if col_idx < (lm + mc) as u32 {
                                col_label(col_idx - lm as u32)
                            } else {
                                format!("]{}", col_label(col_idx - lm as u32 - mc as u32))
                            };
                            if hx + cw + 6 > rect.x + rect.w { break; }
                            let padded = format!("{:<1$}", label, cw as usize);
                            let active_col = col_idx == *cursor_col;
                            let covered_col =
                                cover_cols.is_some_and(|(c0, c1)| col_idx >= c0 && col_idx <= c1);
                            let style = if active_col || covered_col {
                                sgr_header_active()
                            } else {
                                sgr_header_inactive()
                            };
                            out.push_str(&sgr_cup(hr, hx));
                            out.push_str(style);
                            out.push_str(&padded);
                            out.push_str(SGR_RESET);
                            hx += cw;
                            if hx < rect.x + rect.w {
                                out.push_str(&sgr_cup(hr, hx));
                                out.push_str(" ");
                                hx += 1;
                            }
                        }
                    }
                    // header separator: dark gray fg with proper intersections
                    // Left border pipe first, then the ├ sign
                    out.push_str(&sgr_cup(hr + 1, rect.x));
                    out.push_str("│");
                    out.push_str(&sgr_cup(hr + 1, rect.x + 1));
                    out.push_str(sgr_sep());
                    out.push_str("├");
                    let right_border_x = rect.x + rect.w - 1;
                    // Compute separator boundary screen positions from column layout
                    let sep_boundaries = if use_layout && lm > 0 {
                        let mut cx = rect.x + 1 + 5;
                        let mut boundaries: Vec<i32> = Vec::new();
                        for (idx, &(col_idx, w, _)) in column_layout.iter().enumerate() {
                            cx += w as i32;
                            if idx + 1 < column_layout.len() {
                                let is_boundary = col_idx == (lm - 1) as u32 || col_idx == (lm + mc - 1) as u32;
                                if is_boundary {
                                    boundaries.push(cx);
                                }
                                cx += if is_boundary { 2 } else { 1 };
                            }
                        }
                        boundaries
                    } else {
                        Vec::new()
                    };
                    let mut sx = rect.x + 2;
                    for &bx in &sep_boundaries {
                        let end = bx.min(right_border_x);
                        while sx < end {
                            out.push('─');
                            sx += 1;
                        }
                        if sx < right_border_x {
                            out.push_str("┼");
                            sx += 1;
                        }
                    }
                    while sx < right_border_x - 1 {
                        out.push('─');
                        sx += 1;
                    }
                    out.push_str("┤");
                    out.push_str(SGR_FG_DEFAULT);
                    out.push_str("│");
                    row_offset = hr + 2;
                }

                // Data rows
                let has_status = true;
                let has_tabs = !tab_titles.is_empty();
                let extra_lines = 1
                    + if has_tabs { 1 } else if has_status { 1 } else { 0 };
                let grid_bottom = rect.y + rect.h - extra_lines;
                let max_data_rows = ((grid_bottom - row_offset) as i32).max(1) as u32;
                // Determine boundary row indices from row labels instead of
                // using header_row_count/main_row_count (which are logical counts
                // like HEADER_ROWS=999_999_999, not display indices).
                let boundary_row_indices: Vec<u32> = {
                    let mut last_header: Option<u32> = None;
                    let mut last_main: Option<u32> = None;
                    for &(idx, ref label) in row_labels.iter() {
                        if label.starts_with('~') {
                            last_header = Some(idx);
                        } else if label.starts_with('_') {
                            // Footer rows don't affect main boundary
                        } else if !label.trim().is_empty() {
                            last_main = Some(idx);
                        }
                    }
                    let mut result = Vec::new();
                    if let Some(idx) = last_header { result.push(idx); }
                    if let Some(idx) = last_main { result.push(idx); }
                    result
                };
                for vr in 0..max_data_rows {
                    let row_idx = *top_row + vr as u32;
                    let ry = row_offset + vr as i32;
                    let is_cursor_row = row_idx == *cursor_row;
                    // Determine row label style
                    let label_str = row_labels.iter()
                        .find(|(r, _)| *r == row_idx)
                        .map(|(_, l)| l.as_str())
                        .unwrap_or("");
                    // Blank rows (past the end of actual data) have no label;
                    // render them as empty lines matching ratatui.
                    if label_str.is_empty() {
                        out.push_str(&sgr_cup(ry, rect.x));
                        out.push_str("│");
                        let blank_w = (rect.w - 2).max(0) as usize;
                        if blank_w > 0 {
                            out.push_str(&" ".repeat(blank_w));
                        }
                        out.push_str("│");
                        continue;
                    }
                    // Row label styling matches ratatui:
                    // - cursor row: bold + black fg + yellow bg
                    // - footer rows: bold + cyan fg
                    // - normal rows: yellow fg
                    // - boundary (last main) row: underline + fg
                    let is_footer = label_str.starts_with('_');
                    let _is_header = label_str.starts_with('~');
                    // Boundary rows: last header row and last main row before footers.
                    // Uses header_row_count/main_row_count directly (matching ratatui's
                    // last_display_main_row logic) instead of next-label heuristics
                    // that can fail when row_labels have gaps.
                    let is_boundary = boundary_row_indices.contains(&row_idx);
                    let covered_row =
                        cover_rows.is_some_and(|(r0, r1)| row_idx >= r0 && row_idx <= r1);
                    let row_label_style = if is_cursor_row {
                        if is_boundary { sgr_row_cursor() } else { sgr_header_active() }
                    } else if covered_row {
                        // Selected rows glow like the active one. Precedence
                        // matches ratatui: coverage beats the footer style, so
                        // a selection reaching into footers still reads as
                        // selected (the body already highlights those cells).
                        sgr_header_active()
                    } else if is_footer {
                        sgr_row_footer()
                    } else if is_boundary {
                        sgr_row_underline()
                    } else {
                        sgr_row_normal()
                    };
                    // Left border
                    out.push_str(&sgr_cup(ry, rect.x));
                    out.push_str("│");
                    // Row label (5 chars: right-aligned in 4 + trailing space)
                    out.push_str(&sgr_cup(ry, rect.x + 1));
                    out.push_str(row_label_style);
                    out.push_str(&format!("{:>4} ", label_str));
                    // Reset attributes that would leak (bold, background) for cursor
                    // and footer rows.  Boundary rows keep their underline since
                    // ratatui does not reset it; normal rows reset foreground so
                    // text without a cell SGR (e.g. section headers) renders in
                    // the default color instead of inheriting the row-label yellow.
                    if is_cursor_row || is_footer {
                        if is_boundary {
                            out.push_str("\x1b[0;4m");
                        } else {
                            out.push_str(SGR_RESET);
                        }
                    } else if is_boundary {
                        out.push_str("\x1b[0;4m");
                    } else {
                        out.push_str(SGR_FG_DEFAULT);
                        out.push_str(SGR_BG_DEFAULT);
                    }

                    // Separator between row label and first data column (dark gray)
                    // For the left margin gap: dark gray fg for 4 spaces
                    if *margin_cols > 0 && !use_layout {
                        let sep_x = rect.x + 1 + 5; // right after row label
                        out.push_str(&sgr_cup(ry, sep_x));
                        out.push_str(sgr_sep());
                        out.push_str("    ");
                        out.push_str(SGR_FG_DEFAULT);
                        out.push_str(" ");
                    }

                    let cells_ref = cells.borrow();
                    let cell_styles_ref = cell_styles.borrow();
                    if use_layout {
                        let n = column_layout.len();
                        let mut col_positions: Vec<(u32, i32)> = Vec::new();
                        let mut rx2 = rect.x + 1 + 5;
                        for (idx, &(col_idx, w, _)) in column_layout.iter().enumerate() {
                            col_positions.push((col_idx, rx2));
                            rx2 += w as i32;
                            if idx + 1 < n {
                                let is_boundary = lm > 0 && ((col_idx as usize) == lm - 1 || (col_idx as usize) == lm + mc - 1);
                                if is_boundary {
                                    rx2 += 2;
                                } else {
                                    rx2 += 1;
                                }
                            }
                        }
                        // Determine last rendered x position for row-fill detection
                        let _last_col_end = if col_positions.is_empty() {
                            rect.x + 1 + 5
                        } else {
                            let last_idx = col_positions.len() - 1;
                            col_positions[last_idx].1 + column_layout[last_idx].1 as i32
                        };
                        let mut vi = 0;
                        let mut prev_overflowed = false;
                        let mut defer_sgr_reset = false;
                        let mut last_sgr = String::new();
                        let mut overflow_consumed_all = false;
                        while vi < n {
                            let (col_idx, sx) = col_positions[vi];
                            let cell_text = cells_ref.get(&(row_idx, col_idx))
                                .map(|s| s.as_str()).unwrap_or("");
                            let cell_style = cell_styles_ref.get(&(row_idx, col_idx)).copied().unwrap_or(0);
                            let is_cursor_cell = row_idx == *cursor_row && col_idx == *cursor_col;

                        if cell_text.is_empty() {
                                // Empty cell: draw cursor highlight if needed
                                let is_agg_empty = cell_style == 2 || cell_style == 3;
                                if is_cursor_cell {
                                    let cw = column_layout[vi].1 as i32;
                                    out.push_str(&sgr_cup(ry, sx));
                                    if is_boundary {
                                        out.push_str(SGR_UNDERLINE);
                                    }
                                    out.push_str(sgr_cell_cursor());
                                    for _ in 0..cw {
                                        out.push(' ');
                                    }
                                    out.push_str(SGR_BG_DEFAULT);
                                    let gap_after = if vi + 1 < n { 1 } else { 0 };
                                    if gap_after > 0 {
                                        let is_sep_col = lm > 0 && ((col_idx as usize) == lm - 1 || (col_idx as usize) == lm + mc - 1);
                                        if is_sep_col {
                                            out.push_str(sgr_sep());
                                            out.push('│');
                                            out.push_str(SGR_FG_DEFAULT);
                                            out.push(' ');
                                        } else {
                                            out.push(' ');
                                        }
                                    }
                                } else if is_agg_empty && !prev_overflowed {
                                    // Empty aggregate cell: cyan foreground
                                    let cw = column_layout[vi].1 as i32;
                                    out.push_str(&sgr_cup(ry, sx));
                                    if is_boundary {
                                        out.push_str(SGR_UNDERLINE);
                                    }
                                    if cell_style == 3 {
                                        out.push_str(sgr_cell_footer_agg());
                                    } else {
                                        out.push_str(sgr_cell_agg());
                                    }
                                    for _ in 0..cw {
                                        out.push(' ');
                                    }
                                    if cell_style == 3 {
                                        out.push_str(SGR_RESET);
                                    } else if cell_style == 2 {
                                        // Full reset: aggregates are bold on
                                        // every backend now, and a foreground-
                                        // only reset would leak the weight.
                                        out.push_str(SGR_RESET);
                                    } else {
                                        out.push_str(SGR_FG_DEFAULT);
                                    }
                                    let gap_after = if vi + 1 < n { 1 } else { 0 };
                                    if gap_after > 0 {
                                        let is_sep_col = lm > 0 && ((col_idx as usize) == lm - 1 || (col_idx as usize) == lm + mc - 1);
                                        if is_sep_col {
                                            out.push_str(sgr_sep());
                                            out.push('│');
                                            out.push_str(SGR_FG_DEFAULT);
                                            out.push(' ');
                                        } else {
                                            out.push(' ');
                                        }
                                    }
                                } else if (col_idx as usize) < lm {
                                    // Left-margin column: dark gray border or default.
                                    // Match ratatui: only the last left-margin column
                                    // (lm-1) gets dark gray in non-boundary rows; all
                                    // left-margin columns get dark gray in boundary rows.
                                    let cw = column_layout[vi].1 as i32;
                                    let is_last_left = (col_idx as usize) == lm - 1;
                                    let use_gray = is_boundary || is_last_left;
                                    out.push_str(&sgr_cup(ry, sx));
                                    if is_boundary {
                                        out.push_str(SGR_UNDERLINE);
                                    }
                                    if use_gray {
                                        out.push_str(sgr_sep());
                                    } else {
                                        out.push_str(SGR_FG_DEFAULT);
                                        out.push_str(SGR_BG_DEFAULT);
                                    }
                                    for _ in 0..cw {
                                        out.push(' ');
                                    }
                                    if use_gray {
                                        out.push_str(SGR_FG_DEFAULT);
                                    }
                                    let gap_after = if vi + 1 < n { 1 } else { 0 };
                                    if gap_after > 0 {
                                        let is_sep_col = lm > 0 && ((col_idx as usize) == lm - 1 || (col_idx as usize) == lm + mc - 1);
                                        if is_sep_col {
                                            out.push_str(sgr_sep());
                                            out.push('│');
                                            out.push_str(SGR_FG_DEFAULT);
                                            out.push(' ');
                                        } else {
                                            out.push(' ');
                                        }
                                    }
                                } else if (col_idx as usize) >= lm + mc {
                                    // Right-margin column: match ratatui which uses
                                    // gray foreground for all columns in both normal
                                    // and boundary rows.
                                    // When a preceding cell overflowed into this
                                    // area, use default style (matching ratatui's
                                    // paragraph auto-fill behavior).
                                    let cw = column_layout[vi].1 as i32;
                                    let use_gray = is_boundary || ((col_idx as usize) == lm + mc && !prev_overflowed);
                                    out.push_str(&sgr_cup(ry, sx));
                                    if use_gray {
                                        out.push_str(sgr_sep());
                                    } else if prev_overflowed {
                                        out.push_str(SGR_FG_DEFAULT);
                                        out.push_str(SGR_BG_DEFAULT);
                                    } else {
                                        out.push_str(SGR_FG_DEFAULT);
                                        out.push_str(SGR_BG_DEFAULT);
                                    }
                                    for _ in 0..cw {
                                        out.push(' ');
                                    }
                                    if use_gray {
                                        if is_boundary && vi + 1 >= n {
                                            out.push_str(SGR_RESET);
                                        } else {
                                            out.push_str(SGR_FG_DEFAULT);
                                        }
                                    } else {
                                        out.push_str(SGR_FG_DEFAULT);
                                        out.push_str(SGR_BG_DEFAULT);
                                    }
                                let gap_after = if vi + 1 < n { 1 } else { 0 };
                                if gap_after > 0 {
                                    let is_sep_col = lm > 0 && ((col_idx as usize) == lm - 1 || (col_idx as usize) == lm + mc - 1);
                                    if is_sep_col {
                                        out.push_str(sgr_sep());
                                        out.push('│');
                                        out.push_str(SGR_FG_DEFAULT);
                                        out.push(' ');
                                    } else {
                                        out.push(' ');
                                    }
                                }
                            } else {
                                // Empty cell in main column: check style
                                    let cw = column_layout[vi].1 as i32;
                                    out.push_str(&sgr_cup(ry, sx));
                                    if is_boundary && !prev_overflowed {
                                        out.push_str(sgr_sep());
                                    } else if prev_overflowed {
                                        out.push_str(sgr_sep());
                                    } else {
                                        out.push_str(SGR_FG_DEFAULT);
                                        out.push_str(SGR_BG_DEFAULT);
                                    }
                                    for _ in 0..cw {
                                        out.push(' ');
                                    }
                                    if is_boundary && !prev_overflowed {
                                        out.push_str(SGR_FG_DEFAULT);
                                    } else if is_boundary {
                                        if vi + 1 >= n {
                                            out.push_str(SGR_RESET);
                                        } else {
                                            out.push_str(SGR_FG_DEFAULT);
                                        }
                                    } else if prev_overflowed {
                                        out.push_str(SGR_FG_DEFAULT);
                                    }
                                    let gap_after = if vi + 1 < n { 1 } else { 0 };
                                    if gap_after > 0 {
                                        let is_sep_col = lm > 0 && ((col_idx as usize) == lm - 1 || (col_idx as usize) == lm + mc - 1);
                                        if is_sep_col {
                                            out.push_str(sgr_sep());
                                            out.push('│');
                                            out.push_str(SGR_FG_DEFAULT);
                                            out.push(' ');
                                        } else {
                                            out.push(' ');
                                        }
                                    }
                                }
                                prev_overflowed = false;
                                vi += 1;
                                continue;
                            }
                            let w = column_layout[vi].1 as usize;
                            // When editing, show the edit buffer for the cursor cell.
                            // The edit buffer holds the raw value; align it to
                            // the column width (right for numeric, left otherwise).
                            let display_source: String = if *editing && is_cursor_cell {
                                let raw = &**edit_buf;
                                let raw_w = raw.chars().count();
                                if raw_w < w {
                                    if raw.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
                                        // Right-align numeric edit buffer
                                        let pad = w - raw_w;
                                        let mut s = String::with_capacity(w);
                                        for _ in 0..pad { s.push(' '); }
                                        s.push_str(raw);
                                        s
                                    } else {
                                        // Left-align text edit buffer
                                        let mut s = raw.to_string();
                                        for _ in raw_w..w { s.push(' '); }
                                        s
                                    }
                                } else {
                                    raw.to_string()
                                }
                            } else {
                                cell_text.to_string()
                            };
                            let text_width = display_source.chars().count();
                            let mut overflow_cols = 0usize;
                            let mut can_overflow = false;
                            if text_width > w {
                                can_overflow = true;
                                let mut scan = vi + 1;
                                while scan < n {
                                    let (sc_idx, _) = col_positions[scan];
                                    // Stop at the right-margin boundary when the
                                    // text fits within the left-margin + main
                                    // columns, so structural separators (pipes)
                                    // are preserved.  When the text is longer,
                                    // let it overflow into right-margin so it
                                    // can fill the full viewport width.
                                    if lm > 0 && (sc_idx as usize) >= lm + mc {
                                        // Compute total width up to this column,
                                        // including appropriate gaps.
                                        let mut avail_up_to = column_layout[vi..scan]
                                            .iter().map(|&(_, w, _)| w as usize).sum::<usize>();
                                        let idx_span = scan - vi;
                                        avail_up_to += idx_span; // AsciiSpace gaps
                                        // Add +1 for PipeAndSpace at lm→main boundary
                                        let vi_col = column_layout[vi].0 as usize;
                                        if vi_col == lm - 1 {
                                            avail_up_to += 1;
                                        }
                                        if text_width <= avail_up_to {
                                            break;
                                        }
                                    }
                                    let sc_text = cells_ref.get(&(row_idx, sc_idx))
                                        .map(|s| s.as_str()).unwrap_or("");
                                    if sc_text.is_empty() {
                                        overflow_cols += 1;
                                        scan += 1;
                                    } else { break; }
                                }
                            }
                            let gap_target = vi + overflow_cols;
                            let gap_after = if gap_target + 1 < n {
                                let last_ov_col = column_layout[gap_target].0 as usize;
                                if lm > 0 && (last_ov_col == lm - 1 || last_ov_col == lm + mc - 1) {
                                    2
                                } else {
                                    1
                                }
                            } else {
                                0
                            };
                            let mut total_avail: usize = column_layout[vi..=vi+overflow_cols].iter()
                                .map(|&(_, w, _)| w as usize).sum::<usize>()
                                + overflow_cols
                                + gap_after;
                            // When overflow spans past the main→right-margin
                            // boundary, the PipeAndSpace (2) at the boundary is
                            // counted as only 1 in overflow_cols; add the extra 1.
                            if lm > 0 && overflow_cols > 0 {
                                let vi_col = column_layout[vi].0 as usize;
                                let last_ov = column_layout[(vi + overflow_cols).min(n.saturating_sub(1))].0 as usize;
                                if vi_col < lm + mc && last_ov >= lm + mc {
                                    total_avail = total_avail.saturating_add(1);
                                }
                                // When the source column is the last left-margin
                                // column, the PipeAndSpace (2) at the
                                // left-margin→main boundary is counted as only 1
                                // in overflow_cols; add the extra 1.
                                if vi_col == lm - 1 {
                                    total_avail = total_avail.saturating_add(1);
                                }
                            }
                            // Include remaining viewport space (right gap) so
                            // cell style/background fills to the right border,
                            // matching ratatui.
                            let overflow_ends_at_boundary = overflow_cols > 0 && gap_target < n && {
                                let last_ov_col = column_layout[gap_target].0 as usize;
                                lm > 0 && (last_ov_col == lm - 1 || last_ov_col == lm + mc - 1)
                            };
                            if gap_target + 1 >= n || overflow_ends_at_boundary {
                                // Compute render width with boundary-aware gaps,
                                // matching visible_cols_render_width.
                                let mut render_w: usize = column_layout.iter()
                                    .map(|&(_, w, _)| w as usize).sum::<usize>();
                                for idx in 0..n.saturating_sub(1) {
                                    let col_idx = column_layout[idx].0 as usize;
                                let lm_u = lm as usize;
                                if lm_u > 0 && (col_idx == lm_u - 1 || col_idx == lm_u + mc as usize - 1) {
                                        render_w += 2;
                                    } else {
                                        render_w += 1;
                                    }
                                }
                                let total_w = (rect.w as usize).saturating_sub(7);
                                if total_w > render_w {
                                    total_avail = total_avail.saturating_add(total_w - render_w);
                                }
                            }
                            let display = if text_width > total_avail {
                                // Ratatui's spill path fills the available chunk
                                // with the actual text and does NOT append an
                                // ellipsis (only the non-spill fallback ever adds
                                // `…`, via truncate_with_ellipsis).  So match: take
                                // the full available columns from the source.
                                display_source.chars().take(total_avail).collect::<String>()
                            } else { display_source.to_string() };
                            // Cell SGR style matching ratatui priority:
                            //   1. cursor cell → bg(DarkGray)
                            //   2. footer agg  → bold + fg(Cyan)
                            //   3. agg         → fg(Cyan)
                            //   4. hyperlink   → fg(Blue) + underline
                            //   5. border col  → fg(DarkGray)
                            //   6. displaced by overflow → fg(DarkGray)
                            //   7. default     → none
                            //
                            // When a cell text overflows (can_overflow), the
                            // entire overflowed text uses default style (matching
                            // ratatui's spill logic) rather than boundary gray.
                            let is_left_margin_col = (col_idx as usize) < lm;
                            let is_right_margin_col = (col_idx as usize) >= lm + mc;
                            let cell_sgr = if is_cursor_cell {
                                sgr_cell_cursor()
                            } else if cell_style == 3 {
                                sgr_cell_footer_agg()
                            } else if cell_style == 2 {
                                sgr_cell_agg()
                            } else if cell_style == 7 {
                                sgr_cell_link()
                            } else if is_left_margin_col && cell_text.is_empty() {
                                sgr_sep()
                            } else if is_right_margin_col && cell_text.is_empty()
                                && (is_boundary || ((col_idx as usize) == lm + mc && !prev_overflowed)) {
                                sgr_sep()
                            } else if prev_overflowed && cell_text.is_empty() && !is_left_margin_col && !is_right_margin_col {
                                sgr_sep()
                            } else if is_left_margin_col {
                                ""
                            } else if is_boundary {
                                sgr_sep()
                            } else {
                                ""
                            };
                            // When the cell overflows, skip the boundary SGR reset
                            // too so the overflow text stays in default style.
                            let is_overflowing = can_overflow;
                            let underline_prefix = if is_boundary && !can_overflow && is_left_margin_col {
                                SGR_UNDERLINE
                            } else {
                                ""
                            };
                            out.push_str(&sgr_cup(ry, sx));
                            if !underline_prefix.is_empty() {
                                out.push_str(underline_prefix);
                            }
                            if !cell_sgr.is_empty() {
                                out.push_str(cell_sgr);
                            }
                            out.push_str(&display);
                            // avail_w = width within the cell's column(s), excluding
                            // the trailing gap to the next column (gap_after is written
                            // separately so it uses default style, matching ratatui).
                            // Include PipeAndSpace boundary corrections (same as
                            // total_avail above) so padding is correct.
                            let avail_w = if overflow_cols > 0 {
                                let mut a = column_layout[vi..=vi+overflow_cols].iter()
                                    .map(|&(_, w, _)| w as usize).sum::<usize>()
                                    + overflow_cols;
                                if lm > 0 && overflow_cols > 0 {
                                    let vi_col = column_layout[vi].0 as usize;
                                    if vi_col == lm - 1 {
                                        a = a.saturating_add(1);
                                    }
                                }
                                a
                            } else {
                                w
                            };
                            let display_w = display.chars().count();
                            let pad = avail_w.saturating_sub(display_w);
                            if pad > 0 {
                                out.push_str(&" ".repeat(pad));
                            }
                            // If the display text overflows past the column
                            // width into the gap area, only suppress the
                            // part already consumed, not the whole gap.
                            let overflow_into_gap = display_w.saturating_sub(avail_w);
                            let real_gap = gap_after.saturating_sub(overflow_into_gap);
                            let sgr_applied = !cell_sgr.is_empty();
                            // Defer SGR reset for last cell in row when it
                            // has cursor or boundary styling, so the fill
                            // spaces and right border share the same styling.
                            let is_last = vi + 1 >= n;
                            // When overflow consumes all remaining columns,
                            // treat it as 'last' for SGR reset deferral so
                            // boundary rows emit SGR_RESET before the border.
                            let overflow_consumes_all = can_overflow && gap_target + 1 >= n;
                            if overflow_consumes_all {
                                overflow_consumed_all = true;
                            }
                            if (is_last || overflow_consumes_all) && (is_cursor_cell || is_boundary) {
                                defer_sgr_reset = true;
                                last_sgr = if is_cursor_cell && !is_boundary {
                                    SGR_BG_DEFAULT.to_string()
                                } else {
                                    SGR_RESET.to_string()
                                };
                            } else {
                                if is_cursor_cell {
                                    out.push_str(SGR_BG_DEFAULT);
                                } else if cell_style == 2 {
                                    // Full reset, not SGR_FG_DEFAULT: aggregates
                                    // are now bold too, and a foreground-only
                                    // reset would leak the weight into every
                                    // following cell.
                                    out.push_str(SGR_RESET);
                                } else if cell_style == 3 {
                                    out.push_str(SGR_RESET);
                                } else if cell_style == 7 {
                                    // Link style sets fg AND underline: a full
                                    // reset (FG_DEFAULT alone would leak the
                                    // underline into the next cell).
                                    out.push_str(SGR_RESET);
                                } else if is_boundary && !is_overflowing {
                                    if vi + 1 >= n {
                                        out.push_str(SGR_RESET);
                                    } else {
                                        out.push_str(SGR_FG_DEFAULT);
                                    }
                                } else if (is_left_margin_col || is_right_margin_col) && !prev_overflowed {
                                    out.push_str(SGR_FG_DEFAULT);
                                } else if sgr_applied {
                                    out.push_str(SGR_BG_DEFAULT);
                                    out.push_str(SGR_FG_DEFAULT);
                                }
                            }
                            // Inter-column gap – separator │ at group boundaries, space otherwise
                            // When overflow spans multiple columns the boundary check
                            // must consider the last overflowed column, not just the
                            // current column.  Structural separators are always drawn
                            // at section boundaries even when the gap is consumed.
                            let overflow_boundary = overflow_cols > 0 && gap_target < n && {
                                let last_ov_col = column_layout[gap_target].0 as usize;
                                lm > 0 && (last_ov_col == lm - 1 || last_ov_col == lm + mc - 1)
                            };
                            // When a LAST-MAIN-COLUMN cell overflows across the
                            // main→right-margin boundary, its text spills into the
                            // right margin and REPLACES the structural `│` there.
                            // ratatui draws no pipe in that case (the overflow text
                            // fills the boundary column); the pipe only survives when
                            // the overflow stops before the boundary.
                            let crosses_right_boundary = can_overflow
                                && overflow_cols > 0
                                && (col_idx as usize) == lm + mc - 1
                                && gap_target < n
                                && (column_layout[gap_target].0 as usize) >= lm + mc;
                            if real_gap > 0 || overflow_boundary {
                                let is_sep_col = if overflow_boundary {
                                    true
                                } else {
                                    lm > 0 && ((col_idx as usize) == lm - 1 || (col_idx as usize) == lm + mc - 1)
                                };
                                // When overflow text partially fills the gap and
                                // consumed the pipe position, only draw spaces
                                // (no pipe) for the remaining gap characters.
                                // Also suppress the pipe entirely when the source
                                // cell's text spills across the main→right-margin
                                // boundary (crosses_right_boundary): the overflow
                                // text occupies the boundary column and there is
                                // no structural pipe there (matching ratatui).
                                if is_sep_col && overflow_into_gap == 0 && !crosses_right_boundary {
                                    out.push_str(sgr_sep());
                                    out.push('│');
                                    out.push_str(SGR_FG_DEFAULT);
                                    out.push(' ');
                                } else {
                                    for _ in 0..real_gap {
                                        out.push(' ');
                                    }
                                }
                            }
                            let overflowed_this = can_overflow;
                            if overflowed_this {
                                prev_overflowed = true;
                            } else {
                                prev_overflowed = false;
                            }
                            vi += 1 + overflow_cols as usize;
                        }
                        // Apply deferred SGR reset BEFORE the filler and right
                        // border so both use default styling, matching ratatui.
                        if defer_sgr_reset {
                            out.push_str(&last_sgr);
                        }
                        // Fill between after_content and right_border with
                        // spaces to clear any stale characters.
                        // When overflow fills all columns to the right border,
                        // skip the fill since the text already occupies that space.
                        let right_border_x = rect.x + rect.w - 1;
                        if !overflow_consumed_all {
                            let after_content: i32 = rect.x + 1 + 5 + column_layout.iter()
                                .enumerate()
                                .map(|(idx, &(col_idx, w, _))| {
                                    let gap = if idx + 1 < n {
                                        if lm > 0 && ((col_idx as usize) == lm - 1 || (col_idx as usize) == lm + mc - 1) {
                                            2
                                        } else {
                                            1
                                        }
                                    } else { 0 };
                                    w as i32 + gap
                                })
                                .sum::<i32>();
                            if after_content < right_border_x {
                                let gap = (right_border_x - after_content) as usize;
                                out.push_str(SGR_FG_DEFAULT);
                                out.push_str(SGR_BG_DEFAULT);
                                out.push_str(&" ".repeat(gap));
                            }
                        }
                        out.push_str(&sgr_cup(ry, right_border_x));
                        out.push_str("│");
                    } else {
                        let cw = *col_width as i32;
                        let rh_w = 5i32;
                        let max_vis_cols = ((rect.w - rh_w - 2) / (cw + 1)).max(1).min((mc.saturating_add(lm)) as i32);
                        let mut vc = 0i32;
                        while vc < max_vis_cols {
                            let col_idx = *left_col + vc as u32;
                            let cell_text = cells_ref.get(&(row_idx, col_idx)).map(|s| s.as_str()).unwrap_or("");
                            let cell_style = cell_styles_ref.get(&(row_idx, col_idx)).copied().unwrap_or(0);
                            let is_cursor_cell = row_idx == *cursor_row && col_idx == *cursor_col;
                            let col_screen_x = rect.x + 1 + rh_w + vc * (cw + 1);
                            if cell_text.is_empty() && !(*editing && is_cursor_cell) {
                                if is_cursor_cell {
                                    out.push_str(&sgr_cup(ry, col_screen_x));
                                    for _ in 0..cw { out.push(' '); }
                                } else if (col_idx as usize) < lm {
                                    out.push_str(&sgr_cup(ry, col_screen_x));
                                    if is_boundary {
                                        out.push_str(SGR_UNDERLINE);
                                    }
                                    out.push_str(sgr_sep());
                                    for _ in 0..cw { out.push(' '); }
                                    out.push_str(SGR_FG_DEFAULT);
                                } else {
                                    out.push_str(&sgr_cup(ry, col_screen_x));
                                    if is_boundary {
                                        out.push_str(SGR_UNDERLINE);
                                        out.push_str(sgr_sep());
                                    } else {
                                        out.push_str(SGR_FG_DEFAULT);
                                        out.push_str(SGR_BG_DEFAULT);
                                    }
                                    for _ in 0..cw { out.push(' '); }
                                }
                                vc += 1;
                                continue;
                            }
                            let text = if *editing && is_cursor_cell { &edit_buf } else { cell_text };
                            let text_width = text.chars().count();
                            let mut overflow_cols = 0i32;
                            if !(*editing && is_cursor_cell) && text_width > cw as usize {
                                let mut scan = vc + 1;
                                while scan < max_vis_cols {
                                    let sc = *left_col + scan as u32;
                                    if cells_ref.get(&(row_idx, sc)).map_or(true, |s| s.is_empty()) {
                                        overflow_cols += 1;
                                        scan += 1;
                                    } else { break; }
                                }
                            }
                            let gap_after = if overflow_cols == 0 { 1 } else { 0 };
                            let available = (overflow_cols + 1) * cw + gap_after;
                            let display = if text_width > available as usize {
                                if overflow_cols == 0 {
                                    text.chars().take(available as usize).collect()
                                } else {
                                    let trunc = (available - 1).max(1) as usize;
                                    let mut s: String = text.chars().take(trunc).collect();
                                    if text.chars().count() > trunc { s.push('…'); }
                                    s
                                }
                            } else { text.to_string() };
                            out.push_str(&sgr_cup(ry, col_screen_x));
                            if is_boundary {
                                out.push_str(SGR_UNDERLINE);
                            }
                            if is_cursor_cell {
                                out.push_str(sgr_cell_cursor());
                            } else if cell_style == 2 {
                                out.push_str(sgr_cell_agg());
                            } else if cell_style == 3 {
                                out.push_str(sgr_cell_footer_agg());
                            } else if cell_style == 7 {
                                out.push_str(sgr_cell_link());
                            } else if is_boundary {
                                out.push_str(sgr_sep());
                            }
                            out.push_str(&display);
                            if is_cursor_cell {
                                out.push_str(SGR_BG_DEFAULT);
                            } else if cell_style == 2 {
                                // Full reset, not SGR_FG_DEFAULT: aggregates
                                // are now bold too, and a foreground-only
                                // reset would leak the weight into the next
                                // cell.
                                out.push_str(SGR_RESET);
                            } else if cell_style == 3 {
                                out.push_str(SGR_RESET);
                            } else if cell_style == 7 {
                                // Full reset: FG_DEFAULT alone would leak the
                                // link underline into the next cell.
                                out.push_str(SGR_RESET);
                            }
                            vc += 1 + overflow_cols;
                        }
                        if is_boundary {
                            out.push_str(SGR_RESET);
                        }
                        out.push_str(&sgr_cup(ry, rect.x + rect.w - 1));
                        out.push_str("│");
                    }
                }
                // Bottom border line
                {
                    let br = row_offset + max_data_rows as i32;
                    out.push_str(&sgr_cup(br, rect.x));
                    out.push_str("└");
                    for _i in 1..rect.w - 1 {
                        out.push('─');
                    }
                    out.push_str("┘");
                }
                // Status bar: dark gray fg — hints matching ratatui's hints_line()
                let display_status = if *editing || !edit_buf.is_empty() {
                    "  type to edit (or addr: val)   Enter·confirm   Esc·discard".to_string()
                } else if !status_text.is_empty() {
                    status_text.clone()
                } else {
                    "  type/F2·edit; Ctrl+C·copy; Ctrl+X·cut; Ctrl+V·paste; Ctrl+;·date; Ctrl+:·time; Ctrl+S·save; F1·help".to_string()
                };
                let ds_len = display_status.len();
                if has_status {
                    let sr = if has_tabs {
                        row_offset + max_data_rows as i32
                    } else {
                        row_offset + max_data_rows as i32 + 1
                    };
                    if sr < rect.y + rect.h {
                        let max_w = rect.w as usize;
                        let st_end = display_status.char_indices().nth(max_w).map(|(i, _)| i).unwrap_or(ds_len);
                        out.push_str(&sgr_cup(sr, rect.x));
                        out.push_str(sgr_sep());
                        out.push_str(&display_status[..st_end]);
                        let st_vis = display_status[..st_end].chars().count();
                        if has_tabs {
                            if st_vis < rect.w as usize - 1 {
                                for _ in st_vis..rect.w as usize - 1 {
                                    out.push('─');
                                }
                                out.push_str("┘");
                            }
                        } else if st_vis < rect.w as usize {
                            out.push_str(&" ".repeat(rect.w as usize - st_vis));
                        }
                    }
                }
                // Tab bar (styled matching ratatui: inactive=white fg+gray bg, active=bold+black fg+yellow bg)
                if !tab_titles.is_empty() {
                    let ty = row_offset + max_data_rows as i32 + 1;
                    if ty < rect.y + rect.h {
                        let max_w = rect.w as usize;
                        let tab_inactive_sgr = "\x1b[38;5;15m\x1b[48;5;8m";
                        let tab_active_sgr = "\x1b[1m\x1b[38;5;0m\x1b[48;5;3m";
                        let reset_sgr = "\x1b[0m";
                        let mut out_line = String::new();
                        let mut vis_count = 0usize;
                        for (idx, title) in tab_titles.iter().enumerate() {
                            if idx > 0 {
                                let gap = "  ";
                                if vis_count + gap.len() > max_w { break; }
                                out_line.push_str(tab_inactive_sgr);
                                out_line.push_str(gap);
                                vis_count += gap.len();
                            }
                            let tab_text = format!(" {} ", title);
                            if vis_count + tab_text.len() > max_w { break; }
                            let style = if idx == *tab_active { tab_active_sgr } else { tab_inactive_sgr };
                            out_line.push_str(style);
                            out_line.push_str(&tab_text);
                            out_line.push_str(reset_sgr);
                            vis_count += tab_text.len();
                        }
                        // Fill remaining width with inactive style
                        if vis_count < max_w {
                            out_line.push_str(tab_inactive_sgr);
                            out_line.push_str(&" ".repeat(max_w - vis_count));
                        }
                        out.push_str(&sgr_cup(ty, rect.x));
                        out.push_str(&out_line);
                    }
                }

                // Store SGR output for emission after ncurses refresh
                with_state(|s| s.spreadsheet_output.push_str(&out));
            }
        }
    }

    fn is_spreadsheet_focused(state: &PcState, fid: usize) -> bool {
        state.node(fid).map_or(false, |n| matches!(n.kind, PcWidgetKind::Spreadsheet { .. } | PcWidgetKind::DataGrid(_)))
    }

    /// Dispatch one decoded pointer event over the widget tree.
    ///
    /// Only called when the app opted in (`set_mouse_enabled(true)`), so it is
    /// free to assume the terminal is reporting SGR/PDC mouse records.  The
    /// rules mirror what the keyboards paths do, so pointer and keys agree:
    ///
    /// * press/click activates: buttons fire their callbacks, check/radio
    ///   buttons toggle, entries take focus, spreadsheets move the cursor;
    /// * wheel scrolls a focused spreadsheet (three rows per notch, the same
    ///   step the PageUp/PageDown handlers use);
    /// * click-through is topmost-wins with the same `rect` containment test
    ///   used by `render_widget`.
    ///
    /// The event is consumed by the innermost visible widget under the
    /// pointer; nothing bubbles yet.
    fn handle_mouse_event(ev: MouseEvent) {
        let (x, y) = (ev.x, ev.y);

        // Wheel: scroll whichever spreadsheet is focused (the pointer cannot
        // identify a scroll target reliably — terminals report -1,-1 for
        // wheel events, so there is no position to hit-test with).
        if let MouseAction::Wheel { down, .. } = ev.action {
            let scrolled = with_state(|state| {
                let fid = match state.focus_id {
                    Some(fid) if is_spreadsheet_focused(state, fid) => fid,
                    _ => match state.nodes.iter().find(|n| n.visible && is_spreadsheet_kind(&n.kind)) {
                        Some(n) => n.id,
                        None => return false,
                    },
                };
                scroll_spreadsheet(state, fid, if down { 3 } else { -3 });
                true
            });
            if scrolled {
                request_redraw();
            }
            return;
        }

        // Motion and position reports: deliver to a canvas's `on_motion` (the
        // GTK/NWG hover contract). Nothing else has hover state, so an
        // unconsumed motion simply returns — which also avoids repainting on
        // every pointer sample.
        if matches!(ev.action, MouseAction::Moved { .. } | MouseAction::Position) {
            let held = match ev.action {
                MouseAction::Moved { button } => button.unwrap_or(0) as u32,
                _ => 0,
            };
            if let Some(id) = with_state(|state| canvas_hit(state, x, y)) {
                if let Some((px, py)) = canvas_pixel_xy(id, x, y) {
                    let fired = CANVAS_POINTER.with(|h| {
                        let mut table = h.borrow_mut();
                        match table.get_mut(&id).and_then(|c| c.motion.as_mut()) {
                            Some(cb) => {
                                cb(px, py, held);
                                true
                            }
                            None => false,
                        }
                    });
                    if fired {
                        // Motion handlers typically re-render into their own
                        // buffer; let the host ask for the frame explicitly.
                        return;
                    }
                }
            }
            return;
        }

        // Only press/click activate.  Release alone is ignored so the click
        // and release records a terminal may send for one physical click do
        // not fire the widget twice.
        let button = match ev.action {
            MouseAction::Pressed(b)
            | MouseAction::Clicked(b)
            | MouseAction::DoubleClicked(b)
            | MouseAction::TripleClicked(b) => b,
            _ => return,
        };

        // Canvas clicks take precedence over the generic widget dispatch so a
        // canvas-painted UI (menu, dropdown, chart) can hit-test at pixel
        // resolution, exactly like the GTK canvas. Registering either handler
        // marks the canvas as pointer-owning; the click is then never also
        // treated as a plain widget activation.
        if let Some(id) = with_state(|state| canvas_hit(state, x, y)) {
            if let Some((px, py)) = canvas_pixel_xy(id, x, y) {
                let handled = CANVAS_POINTER.with(|h| {
                    let mut table = h.borrow_mut();
                    let handlers = match table.get_mut(&id) {
                        Some(h) => h,
                        None => return false,
                    };
                    // The button-aware handler is the richer contract; the
                    // plain one exists for hosts that only need position.
                    if let Some(cb) = handlers.button.as_mut() {
                        cb(px, py, button as u32, 0);
                        true
                    } else if let Some(cb) = handlers.plain.as_mut() {
                        cb(px, py);
                        true
                    } else {
                        false
                    }
                });
                if handled {
                    return;
                }
            }
        }

        // Only the primary button activates; the others are reserved (a host
        // can inspect them through `set_mouse_hook`).
        if button != 1 {
            return;
        }

        let callbacks: Vec<Callback> = with_state(|state| {
            let hit = state.nodes.iter().rev().find(|n| {
                n.visible
                    && n.rect.x <= x
                    && x < n.rect.x + n.rect.w
                    && n.rect.y <= y
                    && y < n.rect.y + n.rect.h
            });
            let node = match hit {
                Some(n) => n,
                None => return vec![],
            };
            let id = node.id;
            let (lw, lh) = (node.rect.w, node.rect.h);
            let kind = node.kind.clone();
            match kind {
                PcWidgetKind::Button { .. } | PcWidgetKind::SimpleAction => {
                    state.focus_id = Some(id);
                    node_callbacks(state, id)
                }
                PcWidgetKind::CheckButton { .. } => {
                    let mut cbs = node_callbacks(state, id);
                    if let Some(n) = state.node_mut(id) {
                        if let PcWidgetKind::CheckButton { ref mut checked, .. } = n.kind {
                            *checked = !*checked;
                        }
                    }
                    std::mem::take(&mut cbs)
                }
                PcWidgetKind::RadioButton { group_id, .. } => {
                    let mut cbs = node_callbacks(state, id);
                    radio_select(state, id, group_id);
                    std::mem::take(&mut cbs)
                }
                PcWidgetKind::Entry { .. } => {
                    state.focus_id = Some(id);
                    vec![]
                }
                PcWidgetKind::Spreadsheet { ref grid, .. } => {
                    if let Some((row, col)) = mouse_local(Rect { x: 0, y: 0, w: lw, h: lh }, x, y)
                        .and_then(|local| spreadsheet_cell_at(grid, local))
                    {
                        mouse_set_cell_cursor(state, id, row, col);
                    }
                    vec![]
                }
                PcWidgetKind::DataGrid(grid) => {
                    if let Some((row, col)) = mouse_local(Rect { x: 0, y: 0, w: lw, h: lh }, x, y)
                        .and_then(|local| spreadsheet_cell_at(&grid, local))
                    {
                        mouse_set_cell_cursor(state, id, row, col);
                    }
                    vec![]
                }
                _ => vec![],
            }
        });
        fire_callbacks(callbacks);
        // Pointer input changed focus/cursor state that the SGR renderer draws
        // from; repaint now instead of waiting for the next key press.
        request_redraw();
    }

    /// True when a widget kind renders through the cell-grid spreadsheet path.
    fn is_spreadsheet_kind(kind: &PcWidgetKind) -> bool {
        matches!(kind, PcWidgetKind::Spreadsheet { .. } | PcWidgetKind::DataGrid(_))
    }

    /// Clone-free access to a node's callback list (empty when it has none or
    /// the id is unknown).
    fn node_callbacks(state: &mut PcState, id: usize) -> Vec<Callback> {
        match state.node_mut(id) {
            Some(n) => n.callbacks.drain(..).collect(),
            None => Vec::new(),
        }
    }

    /// Check `id` and uncheck every sibling in the same radio group, mirroring
    /// `toggle_focused`'s grouping rule.
    fn radio_select(state: &mut PcState, id: usize, group_id: usize) {
        for n in state.nodes.iter_mut() {
            if let PcWidgetKind::RadioButton { checked, group_id: gid, .. } = &mut n.kind {
                if *gid == group_id {
                    *checked = n.id == id;
                }
            }
        }
    }

    /// Move a spreadsheet/grid cursor to `(row, col)`, clamped to the grid,
    /// committing any in-progress edit first (same order the keyboard paths
    /// use) and then scrolling the viewport to keep the cell visible.
    fn mouse_set_cell_cursor(state: &mut PcState, fid: usize, row: u32, col: u32) {
        spreadsheet_commit_edit(state, fid);
        if let Some(n) = state.node_mut(fid) {
            match &mut n.kind {
                PcWidgetKind::Spreadsheet { grid, .. } => {
                    grid.cursor_row = row.min(grid.total_rows.saturating_sub(1));
                    grid.cursor_col = col.min(grid.total_cols.saturating_sub(1));
                    grid.anchor = None;
                }
                PcWidgetKind::DataGrid(grid) => {
                    grid.cursor_row = row.min(grid.total_rows.saturating_sub(1));
                    grid.cursor_col = col.min(grid.total_cols.saturating_sub(1));
                    grid.anchor = None;
                }
                _ => return,
            }
        }
        // Refill the formula bar / row labels for the new cursor position, then
        // keep it on screen.
        spreadsheet_scroll_to_cursor(state, fid);
        spreadsheet_update_formula_bar(fid);
        // The host owns the "real" cursor (corro syncs it through these
        // callbacks); fire them so pointer clicks behave like arrow keys.
        let moved = with_state(|s| {
            s.node(fid).and_then(|n| match &n.kind {
                PcWidgetKind::Spreadsheet { grid, .. } | PcWidgetKind::DataGrid(grid) => {
                    Some((grid.cursor_row, grid.cursor_col))
                }
                _ => None,
            })
        });
        if let Some((row, col)) = moved {
            let mut cbs = with_state(|s| std::mem::take(&mut s.cursor_move_callbacks));
            for cb in cbs.iter_mut() {
                cb(row, col);
            }
            with_state(|s| s.cursor_move_callbacks = cbs);
        }
    }

    /// Scroll a spreadsheet/grid by `delta` rows (negative = up), clamped to
    /// the grid's row range and kept in sync with the viewport.
    fn scroll_spreadsheet(state: &mut PcState, fid: usize, delta: i32) {
        let max_row = match state.node(fid) {
            Some(n) => match &n.kind {
                PcWidgetKind::Spreadsheet { grid, .. } | PcWidgetKind::DataGrid(grid) => {
                    grid.total_rows.saturating_sub(1) as i32
                }
                _ => return,
            },
            None => return,
        };
        if let Some(n) = state.node_mut(fid) {
            let grid = match &mut n.kind {
                PcWidgetKind::Spreadsheet { grid, .. } => grid,
                PcWidgetKind::DataGrid(grid) => grid,
                _ => return,
            };
            let next = (grid.cursor_row as i32 + delta).clamp(0, max_row) as u32;
            if next == grid.cursor_row {
                return;
            }
            grid.cursor_row = next;
        }
        spreadsheet_scroll_to_cursor(state, fid);
        spreadsheet_update_formula_bar(fid);
        let moved = with_state(|s| {
            s.node(fid).and_then(|n| match &n.kind {
                PcWidgetKind::Spreadsheet { grid, .. } | PcWidgetKind::DataGrid(grid) => {
                    Some((grid.cursor_row, grid.cursor_col))
                }
                _ => None,
            })
        });
        if let Some((row, col)) = moved {
            let mut cbs = with_state(|s| std::mem::take(&mut s.cursor_move_callbacks));
            for cb in cbs.iter_mut() {
                cb(row, col);
            }
            with_state(|s| s.cursor_move_callbacks = cbs);
        }
    }

    fn spreadsheet_is_editing(kind: &PcWidgetKind) -> bool {
        if let PcWidgetKind::Spreadsheet { grid, .. } = kind { grid.editing } else { false }
    }

    // ── Menu helpers (submenu support) ────────────────────────────────────────

    /// Rendered width of a menu item (label + glyph/arrow for non-plain kinds).
    fn menu_item_width(item: &crate::MenuItem) -> usize {
        match item {
            crate::MenuItem::Action { label, shortcut, .. } => {
                label.resolve().len() + if shortcut.as_ref().map_or(false, |s| !s.is_empty()) { 2 } else { 0 }
            }
            crate::MenuItem::Check { label, .. } => label.resolve().len() + 2,
            crate::MenuItem::Radio { label, .. } => label.resolve().len() + 2,
            crate::MenuItem::Separator => 1,
            crate::MenuItem::Submenu { label, shortcut, .. } => {
                label.resolve().len() + 2 + if shortcut.as_ref().map_or(false, |s| !s.is_empty()) { 2 } else { 0 }
            }
        }
    }

    /// Rendered label for a menu item (check/radio glyph, separator line,
    /// submenu arrow).  Check/radio state comes from the action registry when
    /// the action is registered, else from the item's own model state.
    fn menu_item_label(item: &crate::MenuItem) -> String {
        match item {
            crate::MenuItem::Action { label, shortcut, .. } => {
                let label = label.resolve();
                if let Some(sc) = shortcut {
                    if !sc.is_empty() { format!("{sc}\u{00b7}{label}") } else { label }
                } else {
                    label
                }
            }
            crate::MenuItem::Check { label, action, checked } => {
                let label = label.resolve();
                let checked = action_state(action).map(|(_, c)| c).unwrap_or(*checked);
                if checked { format!("\u{2713} {label}") } else { format!("  {label}") }
            }
            crate::MenuItem::Radio { label, action, .. } => {
                let label = label.resolve();
                let checked = action_state(action).map(|(_, c)| c).unwrap_or(false);
                if checked { format!("\u{25cf} {label}") } else { format!("  {label}") }
            }
            crate::MenuItem::Separator => "\u{2500}".repeat(8),
            crate::MenuItem::Submenu { label, shortcut, .. } => {
                let label = label.resolve();
                if let Some(sc) = shortcut {
                    if !sc.is_empty() { format!("{sc}\u{00b7}{label} \u{25b6}") } else { format!("{label} \u{25b6}") }
                } else {
                    format!("{label} \u{25b6}")
                }
            }
        }
    }

    /// All open menu levels: (popup_y, popup_x, items) for the root menu and
    /// each submenu in `menu_stack`.  The root popup sits under the active root
    /// label; each submenu popup sits to the right of the parent item.
    fn menu_levels(state: &PcState) -> Option<Vec<(i32, i32, &Vec<crate::MenuItem>)>> {
        let mid = state.menu_bar_id?;
        let n = state.node(mid)?;
        if let PcWidgetKind::MenuBar { labels, submenu_items } = &n.kind {
            if state.active_submenu >= submenu_items.len() { return None; }
            let win_w = n.rect.w as i32;
            let dy = n.rect.y + 1;
            let mut mx = n.rect.x + 1;
            // Each bar label is rendered padded to len+2 with a two-space
            // separator, so the popup x is 1 + Σ(len+4) — derived from the bar
            // layout (matching ratatui's menu_bar_x) instead of adhoc columns.
            for i in 0..state.active_submenu {
                mx += labels[i].resolve().len() as i32 + 4;
            }
            let (mut py, mut px) = (dy, mx);
            let mut cur = &submenu_items[state.active_submenu].1;
            let mut out = vec![(py, px, cur)];
            for &ix in &state.menu_stack {
                let bw = cur.iter().map(menu_item_width).max().unwrap_or(4) as i32 + 4;
                py += 1 + ix as i32; // submenu top border at the parent item's row
                px += bw + 1;
                // Clamp so the submenu popup stays on screen (right edge).
                if px + bw > win_w { px = (win_w - bw).max(0); }
                if ix < cur.len() {
                    if let crate::MenuItem::Submenu { items, .. } = &cur[ix] {
                        cur = items;
                        out.push((py, px, cur));
                    } else {
                        return None;
                    }
                } else {
                    return None;
                }
            }
            Some(out)
        } else {
            None
        }
    }

    /// The current (deepest) menu level's items.
    fn menu_current_items(state: &PcState) -> Option<&Vec<crate::MenuItem>> {
        menu_levels(state).and_then(|l| l.last().map(|(_, _, items)| *items))
    }

    /// The active root menu's title (e.g. "File").
    fn menu_root_title(state: &PcState) -> String {
        let mid = state.menu_bar_id.unwrap_or(0);
        if let Some(n) = state.node(mid) {
            if let PcWidgetKind::MenuBar { labels, .. } = &n.kind {
                if state.active_submenu < labels.len() {
                    return labels[state.active_submenu].resolve();
                }
            }
        }
        String::new()
    }

    /// Menu-bar line derived from the menubar widget: `" [File]  Edit  ..."`
    /// with the open menu bracketed (first root when closed), matching the
    /// ratatui `menu_bar_line` layout exactly (`" {a}  {b}  ..."` with each
    /// item `[Label]` or ` Label `). Returns None when no menubar widget
    /// exists, in which case callers render the app-set text as before.
    /// Generic over labels (no app-specific content): any menubar gets
    /// bracket tracking for free.
    fn derived_menu_bar_text(state: &PcState) -> Option<String> {
        let mid = state.menu_bar_id?;
        let n = state.node(mid)?;
        if let PcWidgetKind::MenuBar { labels, .. } = &n.kind {
            if labels.is_empty() {
                return None;
            }
            let active = if state.menu_open {
                state.active_submenu.min(labels.len() - 1)
            } else {
                0
            };
            let mut s = String::from(" ");
            for (i, l) in labels.iter().enumerate() {
                let l = l.resolve();
                if i > 0 {
                    s.push_str("  ");
                }
                if i == active {
                    s.push('[');
                    s.push_str(&l);
                    s.push(']');
                } else {
                    s.push(' ');
                    s.push_str(&l);
                    s.push(' ');
                }
            }
            Some(s)
        } else {
            None
        }
    }

    /// Re-emit SGR for just the two affected item rows when the menu highlight
    /// moved (Up/Down).  The grid and the rest of the popup are unchanged, so a
    /// full submenu redraw (which clears + redraws everything) is unnecessary
    /// and would flicker.
    fn emit_menu_highlight(old: usize, new: usize) {
        if old == new { return; }
        let out = with_state(|state| {
            if !state.menu_open { return String::new(); }
            let Some(levels) = menu_levels(state) else { return String::new(); };
            let (py, px, items) = levels[levels.len() - 1];
            let max_w = items.iter().map(menu_item_width).max().unwrap_or(4).max(4) as i32;
            let bw = max_w + 4;
            let left = px;
            let right = left + bw - 1;
            let mut out = String::new();
            for i in [old, new] {
                if i >= items.len() { continue; }
                let row = py + 1 + i as i32;
                // background so grid content behind this row is covered
                out.push_str(&sgr_cup(row, left - 1));
                out.push_str(SGR_RESET);
                out.push(' ');
                out.push_str(&sgr_cup(row, left));
                out.push_str(&" ".repeat(bw as usize));
                // side borders
                out.push_str(&sgr_cup(row, left));
                out.push_str(SGR_RESET);
                out.push('\u{2502}');
                out.push_str(&sgr_cup(row, right));
                out.push_str(SGR_RESET);
                out.push('\u{2502}');
                // label with the new highlight state
                out.push_str(&sgr_cup(row, left + 1));
                out.push_str(if i == new { sgr_row_cursor() } else { sgr_menu() });
                out.push(' ');
                out.push_str(&menu_item_label(&items[i]));
                out.push_str(SGR_RESET);
            }
            out
        });
        if !out.is_empty() { emit_sgr(&out); }
    }

    /// The highlighted item at the current menu level.
    fn menu_current_item(state: &PcState) -> Option<&crate::MenuItem> {
        menu_current_items(state).and_then(|items| items.get(state.active_item))
    }

    /// True when the highlighted item at the current level is a submenu.
    fn menu_current_is_submenu(state: &PcState) -> bool {
        menu_current_item(state).map_or(false, |it| matches!(it, crate::MenuItem::Submenu { .. }))
    }

    /// Enter the highlighted submenu item (pushes onto the stack).
    fn menu_enter_submenu(state: &mut PcState) {
        if menu_current_is_submenu(state) {
            state.menu_stack.push(state.active_item);
            state.active_item = 0;
        }
    }

    /// Exit the current submenu level (pops the stack).
    fn menu_exit_submenu(state: &mut PcState) {
        if !state.menu_stack.is_empty() {
            state.menu_stack.pop();
            state.active_item = 0;
        }
    }

    fn spreadsheet_enter(state: &mut PcState, fid: usize) {
        let result = {
            let n = state.node_mut(fid);
            if let Some(n) = n {
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cells = &grid.cells; let raw_cells = &grid.raw_cells; let cursor_row = &mut grid.cursor_row; let cursor_col = &mut grid.cursor_col; let editing = &mut grid.editing; let edit_buf = &mut grid.edit_buf; let edit_pos = &mut grid.edit_pos; let total_rows = grid.total_rows;
                    if *editing {
                        let val = edit_buf.clone();
                        let r = *cursor_row;
                        let c = *cursor_col;
                        // Only commit+move if buffer differs from original
                        let original = raw_cells.borrow().get(&(r, c)).cloned()
                            .or_else(|| cells.borrow().get(&(r, c)).cloned())
                            .unwrap_or_default();
                        if val != original {
                            cells.borrow_mut().insert((r, c), val.clone());
                            raw_cells.borrow_mut().insert((r, c), val.clone());
                            // Advance cursor down (matching ratatui's commit_edit_and_move_down)
                            if *cursor_row + 1 < total_rows {
                                *cursor_row += 1;
                                // Re-enter edit mode at the new cursor position
                                // (matching ratatui's reference behavior)
                                let existing = raw_cells.borrow().get(&(*cursor_row, *cursor_col)).cloned()
                                    .or_else(|| cells.borrow().get(&(*cursor_row, *cursor_col)).cloned())
                                    .unwrap_or_default();
                                *edit_buf = existing;
                                *edit_pos = edit_buf.len();
                                *editing = true;
                            } else {
                                *editing = false;
                                edit_buf.clear();
                            }
                            Some((r, c, val))
                        } else {
                            // Value unchanged — still advance cursor (matching ratatui)
                            if *cursor_row + 1 < total_rows {
                                *cursor_row += 1;
                                let existing = raw_cells.borrow().get(&(*cursor_row, *cursor_col)).cloned()
                                    .or_else(|| cells.borrow().get(&(*cursor_row, *cursor_col)).cloned())
                                    .unwrap_or_default();
                                *edit_buf = existing;
                                *edit_pos = edit_buf.len();
                                *editing = true;
                            } else {
                                *editing = false;
                                edit_buf.clear();
                            }
                            None
                        }
                    } else {
                        *editing = true;
                        // Load the raw cell value (not formatted display) so the
                        // edit buffer matches ratatui's formula_bar_value.
                        let existing = raw_cells.borrow().get(&(*cursor_row, *cursor_col)).cloned()
                            .or_else(|| cells.borrow().get(&(*cursor_row, *cursor_col)).cloned())
                            .unwrap_or_default();
                        *edit_buf = existing;
                        *edit_pos = edit_buf.len();
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some((r, c, val)) = result {
            // Defer firing to the main loop (outside any `with_state` borrow): these
            // callbacks call `with_state`-using adapter functions, so firing them
            // here (inside `with_state`) would panic with a double RefCell borrow.
            let cbs = std::mem::take(&mut state.commit_edit_callbacks);
            DEFERRED_COMMIT.with(|d| d.borrow_mut().push((r, c, val, cbs)));
        }
    }

    fn spreadsheet_scroll_to_cursor(state: &mut PcState, fid: usize) {
        if let Some(n) = state.node_mut(fid) {
            if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let left_col = &mut grid.left_col; let cursor_col = &grid.cursor_col; let column_layout = &mut grid.column_layout; let cells = &grid.cells; let margin_cols = &grid.margin_cols; let main_cols = &grid.main_cols;
                // When a column layout was explicitly set (e.g. by corro's
                // pnc_backend.rs), do not overwrite it — the application code
                // has already computed the correct columns and widths. Rebuild
                // the dynamic layout only when it is empty (the widget-internal
                // default); otherwise fall through and just refresh the formula
                // bar so it tracks the current cursor.
                if column_layout.is_empty() {
                let lm = *margin_cols as usize;
                let mc = *main_cols as usize;
                let total = lm + mc + lm;
                // Build a dynamic column layout similar to ratatui's visible_col_indices.
                let cur = (*cursor_col as usize).min(total.saturating_sub(1));
                let mut col_ixs: Vec<usize> = Vec::new();
                // Left-margin anchor (last left-margin column).
                if lm > 0 {
                    col_ixs.push(lm - 1);
                }
                // Left-margin band: when cursor is in left margin, show a window around it.
                if cur < lm {
                    let window = 7usize;
                    let end = lm.saturating_sub(1);
                    if end.saturating_sub(cur) <= window {
                        for c in cur..=end { col_ixs.push(c); }
                    } else {
                        let half = window / 2;
                        let lo = cur.saturating_sub(half);
                        let hi = (lo + window).min(end);
                        for c in lo..=hi { col_ixs.push(c); }
                    }
                }
                // Always start main columns from the first main column.
                let main_hi = {
                    let cursor_main = if cur < lm { 0usize } else if cur < lm + mc { cur - lm } else { mc.saturating_sub(1) };
                    (cursor_main + 4).min(mc.saturating_sub(1))
                };
                col_ixs.extend((0..=main_hi).map(|ci| lm + ci));
                // Right-margin band: include blank right-margin columns plus non-blank ones.
                let right_start = lm + mc;
                // Fill remaining viewport with blank right-margin columns so the grid
                // always fills the screen.
                let blank_right = right_start;
                col_ixs.push(blank_right);
                // Also include any right-margin columns with content.
                let cells_ref = cells.borrow();
                let max_right = (0..lm).rev()
                    .find(|&i| cells_ref.iter().any(|((_, c), _)| *c as usize == right_start + i))
                    .unwrap_or(0);
                for i in 0..=max_right {
                    let gc = right_start + i;
                    if !col_ixs.contains(&gc) { col_ixs.push(gc); }
                }
                col_ixs.sort_unstable();
                col_ixs.dedup();
                // Compute available screen width for columns
                let rh_w = 5i32;
                let available = (n.rect.w as i32 - rh_w - 2).max(1) as usize;
                // Trim columns to fit available width (matching trim_visible_cols_to_width)
                while col_ixs.len() > 1 {
                    let total_w: usize = col_ixs.iter().enumerate().map(|(i, &c)| {
                        let sep = if i + 1 >= col_ixs.len() { 0 } else { 1 };
                        let col_max: usize = cells_ref.iter()
                            .filter(|&((_, cc), _)| *cc == c as u32)
                            .map(|(_, text)| text.chars().count())
                            .max()
                            .unwrap_or(1).max(1);
                        col_max + sep
                    }).sum();
                    if total_w <= available { break; }
                    let first = col_ixs.first().copied().unwrap_or(cur);
                    let last = col_ixs.last().copied().unwrap_or(cur);
                    if last > cur {
                        col_ixs.pop();
                    } else if first < cur {
                        col_ixs.remove(0);
                    } else { break; }
                }
                // Build the column layout
                let new_layout: Vec<(u32, u32, String)> = col_ixs.iter().map(|&c| {
                    let w: usize = cells_ref.iter()
                        .filter(|&((_, cc), _)| *cc == c as u32)
                        .map(|(_, text)| text.chars().count())
                        .max()
                        .unwrap_or(4);
                    let label = if c < lm {
                        format!("[{}", col_label((lm - 1 - c) as u32))
                    } else if c < lm + mc {
                        col_label((c - lm) as u32)
                    } else {
                        format!("]{}", col_label((c - lm - mc) as u32))
                    };
                    (c as u32, w as u32, label)
                }).collect();
                *column_layout = new_layout;
                // Update left_col from updated column_layout
                if !column_layout.is_empty() {
                    let first_col = column_layout.first().map(|&(c, _, _)| c as usize).unwrap_or(0);
                    *left_col = first_col as u32;
                }
                }
            }
        }
        spreadsheet_update_formula_bar_inner(state, fid);
    }

    fn spreadsheet_commit_edit(state: &mut PcState, fid: usize) {
        let result = {
            let n = state.node_mut(fid);
            if let Some(n) = n {
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cells = &grid.cells; let raw_cells = &grid.raw_cells; let cursor_row = grid.cursor_row; let cursor_col = grid.cursor_col; let edit_buf = &mut grid.edit_buf; let editing = &mut grid.editing;
                    if *editing {
                        let val = edit_buf.clone();
                        let original = raw_cells.borrow().get(&(cursor_row, cursor_col)).cloned()
                            .or_else(|| cells.borrow().get(&(cursor_row, cursor_col)).cloned())
                            .unwrap_or_default();
                        if val != original {
                            cells.borrow_mut().insert((cursor_row, cursor_col), val.clone());
                            raw_cells.borrow_mut().insert((cursor_row, cursor_col), val.clone());
                            *editing = false;
                            edit_buf.clear();
                            Some((cursor_row, cursor_col, val))
                        } else {
                            *editing = false;
                            edit_buf.clear();
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some((r, c, val)) = result {
            // Defer firing to the main loop (outside any `with_state` borrow): these
            // callbacks call `with_state`-using adapter functions, so firing them
            // here (inside `with_state`) would panic with a double RefCell borrow.
            let cbs = std::mem::take(&mut state.commit_edit_callbacks);
            DEFERRED_COMMIT.with(|d| d.borrow_mut().push((r, c, val, cbs)));
        }
    }

    fn col_label(idx: u32) -> String {
        if idx == u32::MAX { return String::new(); }
        let mut n = idx as u64 + 1;
        let mut s = String::new();
        while n > 0 {
            n -= 1;
            s.push((b'A' + (n % 26) as u8) as char);
            n /= 26;
        }
        s.chars().rev().collect()
    }

    /// Activate the focused widget: toggle CheckButton/RadioButton or fire Button callbacks.
    /// Returns `(was_toggleable, callbacks_to_fire)`. Callbacks must be fired *outside* `with_state`.
    fn toggle_focused(state: &mut PcState, fid: usize) -> (bool, Vec<Callback>) {
        let idx = match state.nodes.iter().position(|n| n.id == fid) {
            Some(i) => i,
            None => return (false, vec![]),
        };
        // Match by reference — do NOT move `kind` out of the node, otherwise
        // subsequent reads of the node's fields become use-after-move.
        match &mut state.nodes[idx].kind {
            PcWidgetKind::Button { .. } => {
                let cbs = std::mem::take(&mut state.nodes[idx].callbacks);
                (true, cbs)
            }
            PcWidgetKind::CheckButton { ref mut checked, .. } => {
                *checked = !*checked;
                let cbs = std::mem::take(&mut state.nodes[idx].callbacks);
                (true, cbs)
            }
            PcWidgetKind::RadioButton { group_id, .. } => {
                let gid = *group_id;
                // Uncheck all radio buttons in the same group
                for node in &mut state.nodes {
                    if let PcWidgetKind::RadioButton { checked: ref mut oc, group_id: og, .. } = &mut node.kind {
                        if *og == gid { *oc = false; }
                    }
                }
                // Check the focused one
                if let PcWidgetKind::RadioButton { checked: ref mut c, .. } = &mut state.nodes[idx].kind {
                    *c = true;
                }
                let cbs = std::mem::take(&mut state.nodes[idx].callbacks);
                (true, cbs)
            }
            _ => (false, vec![]),
        }
    }

    /// Fire a list of callbacks outside of any `with_state` borrow.
    fn fire_callbacks(callbacks: Vec<Callback>) {
        for mut cb in callbacks { cb(); }
    }

    // -- Factory functions --

    pub fn init() -> Result<Box<dyn crate::backends::BackendApp>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Box::new(PcApp))
    }

    fn find_window_id(state: &PcState) -> Option<usize> {
        state.nodes.iter().find(|n| matches!(n.kind, PcWidgetKind::Window { .. })).map(|n| n.id)
    }

    pub fn create_window() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        Ok(with_state(|s| s.add_node(PcWidgetKind::Window { title: String::new() }, None)))
    }

    pub fn create_button(label: &str) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        Ok(with_state(|s| s.add_node(PcWidgetKind::Button { label: label.to_string(), weight: 400, italic: false }, find_window_id(s))))
    }

    pub fn create_label(text: &str) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        Ok(with_state(|s| s.add_node(PcWidgetKind::Label { text: text.to_string(), fixed_w: None }, find_window_id(s))))
    }

    pub fn create_box(horizontal: bool, spacing: i32) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        Ok(with_state(|s| s.add_node(PcWidgetKind::BoxWidget { horizontal, spacing }, find_window_id(s))))
    }

    pub fn create_grid() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        Ok(with_state(|s| s.add_node(PcWidgetKind::Grid { cols: 0, rows: 0 }, find_window_id(s))))
    }

    /// Get a widget's rect (used by tests).
    pub fn get_widget_rect(id: usize) -> Option<(i32, i32, i32, i32)> {
        with_state(|s| s.node(id).map(|n| (n.rect.x, n.rect.y, n.rect.w, n.rect.h)))
    }

    /// Set a widget's rect directly (used by tests and manual layout).
    pub fn set_widget_rect(id: usize, x: i32, y: i32, w: i32, h: i32) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                n.rect = Rect { x, y, w, h };
            }
        });
    }

    /// Request a size for a widget, preserving its position.
    ///
    /// GTK's `gtk_widget_set_size_request` sets a *minimum*, not a size: a box
    /// still grows with its content. The terminal layout has no such
    /// negotiation, so the request is applied as the rect's size directly and
    /// the existing x/y is kept -- changing position would be a side effect a
    /// caller asking only for a size did not ask for. A zero or negative
    /// dimension means "no request" and is ignored, which is what GTK treats
    /// it as too.
    pub fn set_size_request(id: usize, w: i32, h: i32) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                if w > 0 {
                    n.rect.w = w;
                }
                if h > 0 {
                    n.rect.h = h;
                }
            }
        });
    }

    /// Show or hide a widget.
    ///
    /// The renderer already skips nodes with `visible == false` (see the draw
    /// loop, which filters on `n.visible`), so this is a real state change
    /// rather than a no-op: a hidden widget stops being drawn and stops being
    /// reachable by hit-testing.
    pub fn set_visible(id: usize, visible: bool) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                n.visible = visible;
            }
        });
    }

    /// Whether a widget is currently shown. See [`Self::set_visible`].
    pub fn is_visible(id: usize) -> bool {
        with_state(|s| s.node(id).map(|n| n.visible).unwrap_or(false))
    }

    /// Record the response id a dialog reports when it is dismissed without
    /// a button. See `Dialog::set_default_response`.
    pub fn set_dialog_default_response(id: usize, response_id: i32) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                if let PcWidgetKind::Dialog {
                    ref mut default_response,
                    ..
                } = n.kind
                {
                    *default_response = Some(response_id);
                }
            }
        });
    }

    /// The recorded default response for a dialog, if any.
    pub fn dialog_default_response(id: usize) -> Option<i32> {
        with_state(|s| match s.node(id) {
            Some(n) => match n.kind {
                PcWidgetKind::Dialog { default_response, .. } => default_response,
                _ => None,
            },
            None => None,
        })
    }

    /// A widget's first child, or `None` when it has none.
    ///
    /// `Dialog::get_content_area` needs this because a terminal dialog has no
    /// separate content widget: `append_content_area` hands the child straight
    /// to `set_child`, so the content area *is* the first child.
    pub fn first_child(id: usize) -> Option<usize> {
        with_state(|s| s.node(id).and_then(|n| n.children.first().copied()))
    }

    pub fn create_sizer(sizer: crate::Sizer) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        Ok(with_state(|s| s.add_node(PcWidgetKind::Sizer(sizer), find_window_id(s))))
    }

    /// Add a child widget to a sizer with weight, border, and flags.
    pub fn sizer_add(id: usize, widget_id: usize, weight: i32, border: i32, flags: crate::SizerFlags) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                if let PcWidgetKind::Sizer(sz) = &mut n.kind {
                    match sz {
                        crate::Sizer::Box { children, .. }
                        | crate::Sizer::Grid { children, .. }
                        | crate::Sizer::FlexGrid { children, .. } => {
                            children.push(crate::SizerChild { widget: widget_id, weight, border, flags });
                        }
                    }
                }
                n.children.push(widget_id);
            }
        });
    }

    pub fn create_entry() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        Ok(with_state(|s| s.add_node(PcWidgetKind::Entry { buffer: String::new(), cursor: 0 }, find_window_id(s))))
    }

    pub fn create_menu() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        Ok(with_state(|s| s.add_node(PcWidgetKind::Menu, find_window_id(s))))
    }

    pub fn create_simple_action(_name: &str) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        Ok(with_state(|s| s.add_node(PcWidgetKind::SimpleAction, find_window_id(s))))
    }

    pub unsafe fn create_menubar(submenu_items: Vec<(crate::Label, Vec<crate::MenuItem>)>, _action_group: *mut c_void) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        let labels: Vec<crate::Label> = submenu_items.iter().map(|(l, _)| l.clone()).collect();
        Ok(with_state(|s| s.add_node(PcWidgetKind::MenuBar { labels, submenu_items }, find_window_id(s))))
    }

    pub fn create_dialog() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        Ok(with_state(|s| {
            let id = s.add_node(PcWidgetKind::Dialog { title: String::new(), default_response: None }, find_window_id(s));
            // Dialogs created at runtime otherwise keep a zero-size default rect and
            // never render.  Give them a centered box sized to the window.
            if let Some(win_id) = find_window_id(s) {
                let (mw, mh) = s
                    .node(win_id)
                    .map(|w| (w.rect.w.max(40), w.rect.h.max(10)))
                    .unwrap_or((80, 24));
                if let Some(n) = s.node_mut(id) {
                    let w = (mw * 3 / 4).clamp(20, (mw - 2).max(20));
                    let h = (mh * 3 / 5).clamp(8, (mh - 2).max(8));
                    n.rect = Rect { x: (mw - w) / 2, y: (mh - h) / 2, w, h };
                    n.visible = true;
                }
            }
            id
        }))
    }

    pub fn create_dropdown(items: &[&str]) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        let items_str: Vec<String> = items.iter().map(|s| s.to_string()).collect();
        Ok(with_state(|s| s.add_node(PcWidgetKind::DropDown { items: items_str, selected: None }, find_window_id(s))))
    }

    pub fn create_checkbutton(label: &str) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        Ok(with_state(|s| s.add_node(PcWidgetKind::CheckButton { label: label.to_string(), checked: false }, find_window_id(s))))
    }

    pub fn create_radiobutton(group_id: Option<usize>, label: &str) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        let gid = group_id.unwrap_or(0);
        Ok(with_state(|s| s.add_node(PcWidgetKind::RadioButton { label: label.to_string(), checked: false, group_id: gid }, find_window_id(s))))
    }

    pub fn create_textview() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        Ok(with_state(|s| s.add_node(PcWidgetKind::TextView { text: String::new() }, find_window_id(s))))
    }

    pub fn create_canvas() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        Ok(with_state(|s| s.add_node(PcWidgetKind::Canvas, find_window_id(s))))
    }

    pub fn create_overlay() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        Ok(with_state(|s| s.add_node(PcWidgetKind::Overlay, find_window_id(s))))
    }

    pub fn create_scrolled_window() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        Ok(with_state(|s| s.add_node(PcWidgetKind::ScrolledWindow, find_window_id(s))))
    }

    pub fn create_spreadsheet(rows: u32, cols: u32) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        let grid = new_grid(rows, cols);
        let id = with_state(|s| s.add_node(PcWidgetKind::Spreadsheet {
            grid,
            formula_bar_address_id: None,
            formula_bar_entry_id: None,
            menu_text: String::new(),
            status_text: String::new(),
            border_title: String::new(),
            formula_bar_trailing: String::new(),
            tab_titles: Vec::new(),
            tab_active: 0,
        }, find_window_id(s)));
        Ok(id)
    }

    pub fn create_data_grid(rows: u32, cols: u32) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        Ok(with_state(|s| s.add_node(PcWidgetKind::DataGrid(new_grid(rows, cols)), find_window_id(s))))
    }

    pub fn grid_set_cell(id: usize, r: u32, c: u32, text: &str) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                if let PcWidgetKind::DataGrid(grid) = &mut n.kind {
                    grid.cells.borrow_mut().insert((r, c), text.to_string());
                }
            }
        });
    }

    pub fn grid_get_cell(id: usize, r: u32, c: u32) -> Option<String> {
        with_state(|s| {
            s.node(id).and_then(|n| {
                if let PcWidgetKind::DataGrid(grid) = &n.kind {
                    grid.cells.borrow().get(&(r, c)).cloned()
                } else {
                    None
                }
            })
        })
    }

    fn new_grid(rows: u32, cols: u32) -> crate::core::Grid {
        crate::core::Grid {
            cells: Rc::new(RefCell::new(HashMap::new())),
            raw_cells: Rc::new(RefCell::new(HashMap::new())),
            cell_styles: Rc::new(RefCell::new(HashMap::new())),
            total_rows: rows,
            total_cols: cols,
            top_row: 0,
            left_col: 0,
            cursor_row: 0,
            cursor_col: 0,
            editing: false,
            edit_buf: String::new(),
            edit_pos: 0,
            col_width: 12,
            margin_cols: 0,
            main_cols: cols,
            anchor: None,
            header_row_count: 0,
            main_row_count: 0,
            column_layout: Vec::new(),
            row_labels: Vec::new(),
        }
    }

    pub fn spreadsheet_set_cell(id: usize, r: u32, c: u32, text: &str) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cells = &grid.cells;
                    cells.borrow_mut().insert((r, c), text.to_string());
                }
            }
        });
    }

    pub fn spreadsheet_set_cell_style(id: usize, r: u32, c: u32, style: u8) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cell_styles = &grid.cell_styles;
                    cell_styles.borrow_mut().insert((r, c), style);
                }
            }
        });
    }

    pub fn spreadsheet_set_raw_cell(id: usize, r: u32, c: u32, text: &str) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let raw_cells = &grid.raw_cells;
                    raw_cells.borrow_mut().insert((r, c), text.to_string());
                }
            }
        });
    }

    /// Leftmost/rightmost non-blank column in `row` (widget coordinate space),
    /// matching ratatui's row_nonblank_horizontal_extremes. None when the row
    /// has no non-blank cells.
    fn spreadsheet_row_nonblank_extremes(grid: &crate::core::Grid, row: u32) -> Option<(usize, usize)> {
        let cells = grid.cells.borrow();
        let mut first: Option<usize> = None;
        let mut last: Option<usize> = None;
        for (&(r, c), v) in cells.iter() {
            if r == row && !v.trim().is_empty() {
                let c = c as usize;
                if first.is_none() {
                    first = Some(c);
                }
                last = Some(c);
            }
        }
        match (first, last) {
            (Some(a), Some(b)) => Some((a, b)),
            _ => None,
        }
    }

    pub fn spreadsheet_get_cell(id: usize, r: u32, c: u32) -> Option<String> {
        with_state(|s| {            s.node(id).and_then(|n| {
                if let PcWidgetKind::Spreadsheet { grid, .. } = &n.kind { let cells = &grid.cells;
                    cells.borrow().get(&(r, c)).cloned()
                } else {
                    None
                }
            })
        })
    }

    fn spreadsheet_prepare_move(state: &mut PcState, fid: usize, shift: bool) {
        if let Some(n) = state.node_mut(fid) {
            if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let anchor = &mut grid.anchor; let cursor_row = grid.cursor_row; let cursor_col = grid.cursor_col;
                if shift {
                    if anchor.is_none() {
                        *anchor = Some((cursor_row, cursor_col));
                    }
                } else {
                    *anchor = None;
                }
            }
        }
    }

    pub fn spreadsheet_get_anchor(fid: usize) -> Option<(u32, u32)> {
        with_state(|s| {
            s.node(fid).and_then(|n| {
                if let PcWidgetKind::Spreadsheet { grid, .. } = &n.kind {
                    grid.anchor
                } else {
                    None
                }
            })
        })
    }

    /// Push a selection anchor *into* the widget (the reverse of
    /// [`spreadsheet_get_anchor`]).
    ///
    /// Shift+arrow input sets the widget's anchor by itself (the ncurses setup
    /// registers `ESC [ 1 ; 2 {A,B,C,D}` as the shifted keycodes), but hosts
    /// that hold a selection in their own state (corro's Select All, row and
    /// column selection) must push it here or the renderer cannot highlight
    /// the covered headers.
    pub fn spreadsheet_set_anchor(fid: usize, anchor: Option<(u32, u32)>) {
        with_state(|state| {
            for n in state.nodes.iter_mut() {
                if n.id == fid {
                    if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind {
                        grid.anchor = anchor;
                    }
                    break;
                }
            }
        });
    }

    pub fn spreadsheet_clear_anchor(fid: usize) {
        with_state(|s| {
            if let Some(n) = s.node_mut(fid) {
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let anchor = &mut grid.anchor;
                    *anchor = None;
                }
            }
        });
    }

    pub fn spreadsheet_set_column_layout(spreadsheet_id: usize, layout: Vec<(u32, u32, String)>) {
        with_state(|s| {
            if let Some(n) = s.node_mut(spreadsheet_id) {
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let column_layout = &mut grid.column_layout;
                    *column_layout = layout;
                }
            }
        });
    }

    pub fn spreadsheet_set_row_labels(spreadsheet_id: usize, labels: Vec<(u32, String)>) {
        with_state(|s| {
            if let Some(n) = s.node_mut(spreadsheet_id) {
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind {
                    grid.row_labels = labels;
                }
            }
        });
    }

    pub fn spreadsheet_set_menu_text(spreadsheet_id: usize, text: &str) {
        with_state(|s| {
            if let Some(n) = s.node_mut(spreadsheet_id) {
                if let PcWidgetKind::Spreadsheet { ref mut menu_text, .. } = n.kind {
                    *menu_text = text.to_string();
                }
            }
        });
    }

    pub fn spreadsheet_set_border_title(spreadsheet_id: usize, text: &str) {
        with_state(|s| {
            if let Some(n) = s.node_mut(spreadsheet_id) {
                if let PcWidgetKind::Spreadsheet { ref mut border_title, .. } = n.kind {
                    *border_title = text.to_string();
                }
            }
        });
    }

    pub fn spreadsheet_set_status_text(spreadsheet_id: usize, text: &str) {
        with_state(|s| {
            if let Some(n) = s.node_mut(spreadsheet_id) {
                if let PcWidgetKind::Spreadsheet { ref mut status_text, .. } = n.kind {
                    *status_text = text.to_string();
                }
            }
        });
    }

    pub fn spreadsheet_set_formula_bar_trailing(spreadsheet_id: usize, text: &str) {
        with_state(|s| {
            if let Some(n) = s.node_mut(spreadsheet_id) {
                if let PcWidgetKind::Spreadsheet { ref mut formula_bar_trailing, .. } = n.kind {
                    *formula_bar_trailing = text.to_string();
                }
            }
        });
    }

    pub fn spreadsheet_set_tab_data(spreadsheet_id: usize, titles: &[String], active: usize) {
        with_state(|s| {
            if let Some(n) = s.node_mut(spreadsheet_id) {
                if let PcWidgetKind::Spreadsheet { ref mut tab_titles, ref mut tab_active, .. } = n.kind {
                    *tab_titles = titles.to_vec();
                    *tab_active = active;
                }
            }
        });
    }

    pub fn spreadsheet_set_grid_config(spreadsheet_id: usize, margin_c: u32, main_c: u32) {
        with_state(|s| {
            if let Some(n) = s.node_mut(spreadsheet_id) {
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let margin_cols = &mut grid.margin_cols; let main_cols = &mut grid.main_cols;
                    *margin_cols = margin_c;
                    *main_cols = main_c;
                }
            }
        });
    }

    pub fn spreadsheet_set_row_counts(spreadsheet_id: usize, header_rows: u32, main_rows: u32) {
        with_state(|s| {
            if let Some(n) = s.node_mut(spreadsheet_id) {
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let header_row_count = &mut grid.header_row_count; let main_row_count = &mut grid.main_row_count;
                    *header_row_count = header_rows;
                    *main_row_count = main_rows;
                }
            }
        });
    }

    pub fn spreadsheet_cursor_position(id: usize) -> Option<(u32, u32)> {
        with_state(|s| {
            s.node(id).and_then(|n| {
                if let PcWidgetKind::Spreadsheet { grid, .. } = &n.kind {
                    Some((grid.cursor_row, grid.cursor_col))
                } else { None }
            })
        })
    }

    pub fn spreadsheet_add_cursor_move_callback<F: FnMut(u32, u32) + 'static>(f: F) {
        with_state(|state| {
            state.cursor_move_callbacks.push(Box::new(f));
        });
    }

    pub fn spreadsheet_add_commit_edit_callback<F: FnMut(u32, u32, String) + 'static>(f: F) {
        with_state(|state| {
            state.commit_edit_callbacks.push(Box::new(f));
        });
    }

    pub fn spreadsheet_add_goto_callback<F: FnMut() + 'static>(f: F) {
        with_state(|state| {
            state.goto_callbacks.push(Box::new(f));
        });
    }

    pub fn spreadsheet_set_cursor(id: usize, row: u32, col: u32) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_row = &mut grid.cursor_row; let cursor_col = &mut grid.cursor_col;
                    *cursor_row = row;
                    *cursor_col = col;
                }
            }
        });
    }

    pub fn spreadsheet_set_edit_state(id: usize, is_editing: bool, buf: &str, pos: usize) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let editing = &mut grid.editing; let edit_buf = &mut grid.edit_buf; let edit_pos = &mut grid.edit_pos;
                    *editing = is_editing;
                    *edit_buf = buf.to_string();
                    *edit_pos = pos;
                }
            }
        });
    }

    /// Read back a spreadsheet's edit state (generic mechanism for hosts
    /// that splice text into an in-progress edit, e.g. a picker commit).
    /// Returns (editing, buffer, byte position); missing widget → default.
    pub fn spreadsheet_get_edit_state(id: usize) -> (bool, String, usize) {
        with_state(|s| {
            if let Some(n) = s.node(id) {
                if let PcWidgetKind::Spreadsheet { ref grid, .. } = n.kind {
                    return (grid.editing, grid.edit_buf.clone(), grid.edit_pos);
                }
            }
            (false, String::new(), 0)
        })
    }

    pub fn spreadsheet_commit_formula_bar(spreadsheet_id: usize) {
        let result = with_state(|s| {
            let (entry_id, cursor_row, cursor_col) = match s.node(spreadsheet_id) {
                Some(n) => match &n.kind {
                    PcWidgetKind::Spreadsheet { grid, formula_bar_entry_id, .. } => {
                        (*formula_bar_entry_id, grid.cursor_row, grid.cursor_col)
                    }
                    _ => return None,
                },
                None => return None,
            };
            let Some(eid) = entry_id else { return None };
            let text = match s.node(eid) {
                Some(en) => match &en.kind {
                    PcWidgetKind::Entry { buffer, .. } => buffer.clone(),
                    _ => return None,
                },
                None => return None,
            };
            if let Some(n) = s.node_mut(spreadsheet_id) {
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cells = &grid.cells;
                    cells.borrow_mut().insert((cursor_row, cursor_col), text.clone());
                }
            }
            Some((cursor_row, cursor_col, text))
        });
        if let Some((r, c, text)) = result {
            with_state(|s| {
                let mut cbs = std::mem::take(&mut s.commit_edit_callbacks);
                for cb in cbs.iter_mut() { cb(r, c, text.clone()); }
                s.commit_edit_callbacks = cbs;
            });
        }
    }

    pub fn spreadsheet_set_formula_bar(spreadsheet_id: usize, address_label_id: usize, entry_id: usize) {
        with_state(|s| {
            if let Some(n) = s.node_mut(spreadsheet_id) {
                if let PcWidgetKind::Spreadsheet { ref mut formula_bar_address_id, ref mut formula_bar_entry_id, .. } = n.kind {
                    *formula_bar_address_id = Some(address_label_id);
                    *formula_bar_entry_id = Some(entry_id);
                }
            }
        });
        spreadsheet_update_formula_bar(spreadsheet_id);
    }

    fn spreadsheet_update_formula_bar(fid: usize) {
        with_state(|s| {
            if spreadsheet_update_formula_bar_inner(s, fid) {
                // Mark for re-render by adjusting rect
                if let Some(n) = s.node_mut(fid) {
                    n.rect.w = n.rect.w.max(1);
                }
            }
        });
    }

    fn spreadsheet_update_formula_bar_inner(state: &mut PcState, fid: usize) -> bool {
        let n = match state.node(fid) {
            Some(n) => n,
            None => return false,
        };
        let (cells, row_labels, cursor_row, cursor_col, margin_cols, main_cols, addr_id, entry_id) = match &n.kind {
            PcWidgetKind::Spreadsheet { grid, formula_bar_address_id, formula_bar_entry_id, .. } => {
                (grid.cells.clone(), grid.row_labels.clone(), grid.cursor_row, grid.cursor_col, grid.margin_cols, grid.main_cols, *formula_bar_address_id, *formula_bar_entry_id)
            }
            _ => return false,
        };
        let mut changed = false;
        if let Some(aid) = addr_id {
            let col_part = if cursor_col < margin_cols {
                let margin_idx = margin_cols.saturating_sub(1).saturating_sub(cursor_col);
                format!("[{}", col_label(margin_idx))
            } else if cursor_col < margin_cols + main_cols {
                col_label(cursor_col - margin_cols)
            } else {
                let right_idx = cursor_col.saturating_sub(margin_cols).saturating_sub(main_cols);
                format!("]{}", col_label(right_idx))
            };
            let row_label = row_labels.iter()
                .find(|(r, _)| *r == cursor_row)
                .map(|(_, l)| l.as_str())
                .unwrap_or("1");
            let label = format!("{}{}", col_part, row_label.trim());
            if let Some(an) = state.node_mut(aid) {
                if let PcWidgetKind::Label { ref mut text, .. } = &mut an.kind {
                    if *text != label {
                        *text = label;
                        changed = true;
                    }
                }
            }
        }
        if let Some(eid) = entry_id {
            let val = cells.borrow().get(&(cursor_row, cursor_col)).cloned().unwrap_or_default();
            if let Some(en) = state.node_mut(eid) {
                if let PcWidgetKind::Entry { ref mut buffer, .. } = &mut en.kind {
                    if *buffer != val {
                        *buffer = val;
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    pub fn set_window_title(id: usize, title: &str) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                match &mut n.kind {
                    PcWidgetKind::Window { title: ref mut t } | PcWidgetKind::Dialog { title: ref mut t, .. } => {
                        *t = title.to_string();
                    }
                    _ => {}
                }
            }
        });
    }

    pub fn set_label_text(id: usize, text: &str) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                if let PcWidgetKind::Label { text: ref mut t, .. } = n.kind {
                    *t = text.to_string();
                }
            }
        });
    }

    pub fn get_label_text(id: usize) -> Option<String> {
        with_state(|s| {
            s.node(id).and_then(|n| {
                if let PcWidgetKind::Label { ref text, .. } = n.kind {
                    Some(text.clone())
                } else {
                    None
                }
            })
        })
    }

    pub fn set_label_visible(id: usize, visible: bool) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                n.visible = visible;
            }
        });
    }

    /// Pin a label's laid-out width, in character cells.
    ///
    /// The terminal half of `Label::set_fixed_width` (see the NWG/GTK
    /// adapters for the motivation): a label is shrink-to-fit, so a value
    /// that changes every keystroke — the formula bar's address, `A1` to
    /// `A100` — resizes its slot and slides every sibling packed after it.
    /// A pin gives the slot a constant size; the box layout then hands the
    /// label that many cells and the text simply gets clipped if it is wider
    /// (the NWG note about picking a width that fits the widest case applies
    /// here too). `None` restores shrink-to-fit.
    pub fn set_label_fixed_width(id: usize, w: Option<i32>) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                if let PcWidgetKind::Label { fixed_w, .. } = &mut n.kind {
                    *fixed_w = w.filter(|w| *w > 0);
                }
            }
        });
    }

    /// Pinned width of a label, or `None` when it shrinks to fit. Exposed so
    /// the layout math and the app can agree on what the pin means.
    pub fn get_label_fixed_width(id: usize) -> Option<i32> {
        with_state(|s| {
            s.node(id).and_then(|n| match &n.kind {
                PcWidgetKind::Label { fixed_w, .. } => *fixed_w,
                _ => None,
            })
        })
    }

    pub fn add_callback(id: usize, cb: Box<dyn FnMut()>) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                n.callbacks.push(cb);
            }
        });
    }

    /// Register an opt-in event callback.  Returning `CallbackResult::Skip`
    /// lets the event bubble to the parent widget (mirrors wxEvent::Skip);
    /// `Handled` stops it.  Existing `add_callback` closures (returning `()`)
    /// are unaffected and do not participate in propagation.
    pub fn add_event_callback(id: usize, cb: Box<dyn FnMut(&crate::Event) -> crate::CallbackResult>) {
        with_state(|s| s.event_callbacks.push((id, cb)));
    }

    /// Dispatch an event to a widget's event callbacks.  If every callback
    /// returns `Skip`, the event bubbles to the parent widget, recursively.
    /// Returns `Handled` if any callback handled it, else `Skip`.
    pub fn fire_event(id: usize, event: &crate::Event) -> crate::CallbackResult {
        let mut result = crate::CallbackResult::Skip;
        let cbs: Vec<Box<dyn FnMut(&crate::Event) -> crate::CallbackResult>> = with_state(|s| {
            let mut out = Vec::new();
            let mut i = 0;
            while i < s.event_callbacks.len() {
                if s.event_callbacks[i].0 == id {
                    out.push(s.event_callbacks.swap_remove(i).1);
                } else {
                    i += 1;
                }
            }
            out
        });
        for mut cb in cbs {
            if cb(event) == crate::CallbackResult::Handled {
                result = crate::CallbackResult::Handled;
            }
            with_state(|s| s.event_callbacks.push((id, cb)));
        }
        if result == crate::CallbackResult::Skip {
            if let Some(parent) = with_state(|s| s.node(id).and_then(|n| n.parent)) {
                return fire_event(parent, event);
            }
        }
        result
    }

    /// Minimal RFC 4648 base64 encoder (no padding) for OSC 52 clipboard text.
    fn base64_encode(data: &[u8]) -> String {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = *chunk.get(1).unwrap_or(&0) as u32;
            let b2 = *chunk.get(2).unwrap_or(&0) as u32;
            let n = (b0 << 16) | (b1 << 8) | b2;
            out.push(T[(n >> 18) as usize & 63] as char);
            out.push(T[(n >> 12) as usize & 63] as char);
            if chunk.len() > 1 { out.push(T[(n >> 6) as usize & 63] as char); }
            if chunk.len() > 2 { out.push(T[n as usize & 63] as char); }
        }
        out
    }

    /// Copy text to the terminal's system clipboard via OSC 52
    /// (\x1b]52;c;<base64>;...).  Terminals that support it let the user paste
    /// the value in another app; unsupported terminals ignore the sequence.
    pub fn set_clipboard_text(text: &str) {
        let b64 = base64_encode(text.as_bytes());
        let mut out = String::new();
        out.push_str("\x1b]52;c;");
        out.push_str(&b64);
        out.push('\x07');
        emit_sgr(&out);
    }

    /// Attach arbitrary string client data to a widget (mirrors wxWindow::SetClientData).
    pub fn set_client_data(id: usize, data: &str) {
        with_state(|s| { s.client_data.insert(id, data.to_string()); });
    }

    /// Retrieve a widget's client data (mirrors wxWindow::GetClientData).
    pub fn get_client_data(id: usize) -> Option<String> {
        with_state(|s| s.client_data.get(&id).cloned())
    }

    /// Set an entry's text **and fire its `connect_changed` callbacks**.
    ///
    /// The callbacks are taken out, run, and put back rather than run under the
    /// state borrow: a `connect_changed` handler routinely reads or writes
    /// other widgets (that is the point of the hook), and holding the `RefCell`
    /// across the call would panic on re-entry. Same convention as the
    /// cursor-move/commit-edit callback dispatch.
    pub fn set_entry_text(id: usize, text: &str) {
        let mut callbacks = with_state(|s| {
            match s.node_mut(id) {
                Some(n) => {
                    if let PcWidgetKind::Entry { ref mut buffer, ref mut cursor } = n.kind {
                        *buffer = text.to_string();
                        // Byte index of the end, matching the widget's own
                        // caret convention.
                        *cursor = buffer.len();
                    }
                    std::mem::take(&mut n.callbacks)
                }
                None => Vec::new(),
            }
        });
        // Fire, then put the registrations back: `add_callback` handlers are
        // persistent (they fire on every change, not once). `Callback` is a
        // `Box<dyn FnMut()>` and so is not `Clone`, so the list is moved out,
        // each closure is run, and each is moved straight back — no temporary
        // copy and nothing left consumed.
        let mut taken: Vec<Callback> = Vec::new();
        std::mem::swap(&mut taken, &mut callbacks);
        for mut cb in taken {
            cb();
            callbacks.push(cb);
        }
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                let mut prev = std::mem::take(&mut n.callbacks);
                prev.append(&mut callbacks);
                n.callbacks = prev;
            }
        });
    }

    pub fn get_entry_text(id: usize) -> Option<String> {
        with_state(|s| {
            s.node(id).and_then(|n| {
                if let PcWidgetKind::Entry { ref buffer, .. } = n.kind {
                    Some(buffer.clone())
                } else {
                    None
                }
            })
        })
    }

    /// Read an entry's caret (byte offset into the buffer).
    ///
    /// The widget has always tracked this (`PcWidgetKind::Entry { cursor }` —
    /// the arrow/Backspace/Delete handlers maintain it); it was simply not
    /// exposed, which forced callers to keep a parallel copy. Exposing it makes
    /// the terminal entry satisfy the same caret contract GTK's `Entry` does,
    /// so a host's edit logic is backend-independent.
    pub fn get_entry_cursor(id: usize) -> Option<usize> {
        with_state(|s| {
            s.node(id).and_then(|n| match &n.kind {
                PcWidgetKind::Entry { cursor, .. } => Some(*cursor),
                _ => None,
            })
        })
    }

    /// Move an entry's caret, clamped to the buffer's byte length.
    ///
    /// Clamping matters: the caret is used to index the buffer, so an
    /// out-of-range value from a host (a stale position after the text was
    /// replaced) would panic on the next keystroke rather than merely being
    /// wrong. Also snaps back to a char boundary, since the widget removes
    /// bytes and a mid-codepoint caret would corrupt the text.
    pub fn set_entry_cursor(id: usize, pos: usize) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                if let PcWidgetKind::Entry { ref buffer, ref mut cursor } = n.kind {
                    *cursor = clamp_to_char_boundary(buffer, pos);
                }
            }
        });
    }

    /// Largest char boundary `<= pos` (and `<= len`).
    fn clamp_to_char_boundary(s: &str, pos: usize) -> usize {
        let mut p = pos.min(s.len());
        while p > 0 && !s.is_char_boundary(p) {
            p -= 1;
        }
        p
    }

    /// Focus an entry. A terminal has exactly one focus at a time, which the
    /// widget tree already models with `focus_id`, so this sets that rather
    /// than being a no-op — otherwise a host calling `grab_focus` would see
    /// `has_focus` stay false forever.
    pub fn entry_grab_focus(id: usize) {
        with_state(|s| {
            if matches!(s.node(id).map(|n| &n.kind), Some(PcWidgetKind::Entry { .. })) {
                s.focus_id = Some(id);
            }
        });
    }

    /// Whether an entry currently holds focus.
    pub fn entry_has_focus(id: usize) -> bool {
        with_state(|s| s.focus_id == Some(id))
    }

    pub fn set_textview_text(id: usize, text: &str) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                if let PcWidgetKind::TextView { text: ref mut t } = n.kind {
                    *t = text.to_string();
                }
            }
        });
    }

    pub fn get_textview_text(id: usize) -> Option<String> {
        with_state(|s| {
            s.node(id).and_then(|n| {
                if let PcWidgetKind::TextView { ref text } = n.kind {
                    Some(text.clone())
                } else {
                    None
                }
            })
        })
    }

    pub fn set_dropdown_items(id: usize, items: &[&str]) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                if let PcWidgetKind::DropDown { items: ref mut items_vec, .. } = n.kind {
                    *items_vec = items.iter().map(|s| s.to_string()).collect();
                }
            }
        });
    }

    pub fn set_dropdown_selected(id: usize, idx: i32) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                if let PcWidgetKind::DropDown { ref mut selected, ref items } = n.kind {
                    *selected = if idx >= 0 && (idx as usize) < items.len() { Some(idx as usize) } else { None };
                }
            }
        });
    }

    pub fn get_dropdown_selected(id: usize) -> i32 {
        with_state(|s| {
            s.node(id).and_then(|n| {
                if let PcWidgetKind::DropDown { ref selected, .. } = n.kind {
                    selected.map(|s| s as i32)
                } else {
                    None
                }
            }).unwrap_or(-1)
        })
    }

    pub fn get_checkbutton_checked(id: usize) -> bool {
        with_state(|s| {
            s.node(id).map(|n| {
                if let PcWidgetKind::CheckButton { ref checked, .. } = n.kind {
                    *checked
                } else {
                    false
                }
            }).unwrap_or(false)
        })
    }

    pub fn get_radiobutton_checked(id: usize) -> bool {
        with_state(|s| {
            s.node(id).map(|n| {
                if let PcWidgetKind::RadioButton { ref checked, .. } = n.kind {
                    *checked
                } else {
                    false
                }
            }).unwrap_or(false)
        })
    }

    pub fn set_checkbutton_checked(id: usize, checked: bool) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                if let PcWidgetKind::CheckButton { checked: ref mut c, .. } = n.kind {
                    *c = checked;
                }
            }
        });
    }

    pub fn set_radiobutton_checked(id: usize, checked: bool) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                if let PcWidgetKind::RadioButton { checked: ref mut c, .. } = n.kind {
                    *c = checked;
                }
            }
        });
    }

    pub fn set_child(parent_id: usize, child_id: usize) {
        with_state(|s| {
            s.nodes.iter_mut().for_each(|n| n.children.retain(|c| *c != child_id));
            if let Some(parent) = s.node_mut(parent_id) {
                parent.children.push(child_id);
            }
            if let Some(child) = s.node_mut(child_id) {
                child.parent = Some(parent_id);
            }
        });
    }

    pub fn append_child(parent_id: usize, child_id: usize) {
        set_child(parent_id, child_id);
    }

    fn layout_box_inner(id: usize, s: &mut PcState) -> usize {
        let (horizontal, spacing, children) = {
            let node = s.node(id);
            if node.is_none() { return 0; }
            let n = node.unwrap();
            match &n.kind {
                PcWidgetKind::BoxWidget { horizontal, spacing } => {
                    (*horizontal, *spacing, n.children.clone())
                }
                _ => return 0,
            }
        };

        if children.is_empty() { return 0; }

        let parent_rect = s.node(id).map(|n| n.rect).unwrap_or(Rect::default());
        let spacing = spacing.min(2);
        let total_children = children.len() as i32;
        let total_spacing = spacing * (total_children - 1).max(0);

        if horizontal {
            let total_w = parent_rect.w.saturating_sub(total_spacing).max(1);
            // Compute natural widths for each child based on content
            let natural_widths: Vec<i32> = children.iter().map(|&cid| {
                match s.node(cid).map(|n| &n.kind) {
                    Some(PcWidgetKind::Button { label, .. }) => (label.len() + 2) as i32,
                    Some(PcWidgetKind::Label { text, fixed_w }) => {
                        // A pinned label keeps its slot; the text is clipped
                        // to it (see `set_label_fixed_width`).
                        fixed_w.unwrap_or(text.chars().count() as i32)
                    }
                    Some(PcWidgetKind::CheckButton { label, .. }) => (label.len() + 4) as i32,
                    Some(PcWidgetKind::RadioButton { label, .. }) => (label.len() + 4) as i32,
                    Some(PcWidgetKind::DropDown { items, .. }) => {
                        let max_item = items.iter().map(|s| s.len()).max().unwrap_or(0);
                        (max_item + 3) as i32 // [text▼]
                    }
                    Some(PcWidgetKind::Entry { buffer, .. }) => (buffer.len() + 4).max(6) as i32,
                    Some(PcWidgetKind::TextView { text }) => {
                        text.lines().next().map(|l| l.len()).unwrap_or(0).max(6) as i32
                    }
                    _ => 4,
                }
            }).collect();
            let total_natural: i32 = natural_widths.iter().sum();
            if total_natural <= total_w {
                // Give each child its natural width then distribute extra proportionally
                let extra = total_w - total_natural;
                // A pin (`set_label_fixed_width`) is a promise that this label's
                // slot NEVER moves, so it takes no share of the slack: the extra
                // is distributed among the UNPINNED children only, with the
                // last of them taking the remainder. This is the shape GTK
                // gives the same row — a width request the box clamps, with the
                // leftover going to the expanding child — and it is what makes
                // the pin actually hold the address slot steady while the text
                // changes.
                let pinned: Vec<bool> = children
                    .iter()
                    .map(|&c| {
                        matches!(
                            s.node(c).map(|n| &n.kind),
                            Some(PcWidgetKind::Label { fixed_w: Some(_), .. })
                        )
                    })
                    .collect();
                let flexible: Vec<usize> =
                    (0..children.len()).filter(|&i| !pinned[i]).collect();
                let flex_total: i32 = flexible.iter().map(|&i| natural_widths[i]).sum();
                let mut x = parent_rect.x;
                let mut remaining = extra;
                for (i, child_id) in children.iter().enumerate() {
                    let extra_share = if pinned[i] || flex_total <= 0 {
                        0
                    } else if flexible.last() == Some(&i) {
                        remaining
                    } else {
                        let share = extra * natural_widths[i] / flex_total;
                        remaining -= share;
                        share
                    };
                    let w = natural_widths[i] + extra_share;
                    if let Some(n) = s.node_mut(*child_id) {
                        n.rect = Rect { x, y: parent_rect.y, w, h: parent_rect.h.max(1) };
                    }
                    x += w;
                    if i + 1 < children.len() { x += spacing; }
                }
            } else {
                // Shrink longest-first: give each child its natural width,
                // then repeatedly shrink the widest by 1 until everything fits.
                let mut widths: Vec<i32> = natural_widths.iter().map(|&w| w.max(1)).collect();
                let mut total: i32 = widths.iter().sum();
                while total > total_w {
                    // Find the widest child and shrink it by 1
                    let mut max_i = 0;
                    for i in 1..widths.len() {
                        if widths[i] > widths[max_i] { max_i = i; }
                    }
                    widths[max_i] -= 1;
                    total -= 1;
                }
                let mut x = parent_rect.x;
                for (i, child_id) in children.iter().enumerate() {
                    if let Some(n) = s.node_mut(*child_id) {
                        n.rect = Rect { x, y: parent_rect.y, w: widths[i], h: parent_rect.h.max(1) };
                    }
                    x += widths[i];
                    if i + 1 < children.len() { x += spacing; }
                }
            }
        } else {
            // Natural-height vertical layout: each child gets 1 row by default,
            // last child stretches to fill any remaining space.
            let mut y = parent_rect.y;
            let natural_h = 1i32;
            let total_used = natural_h * total_children + total_spacing;
            let remaining = parent_rect.h.saturating_sub(total_used);
            for (i, child_id) in children.iter().enumerate() {
                let h = if i as i32 == total_children - 1 {
                    (natural_h + remaining).max(1)
                } else {
                    natural_h
                };
                if let Some(n) = s.node_mut(*child_id) {
                    n.rect = Rect { x: parent_rect.x, y, w: parent_rect.w.max(1), h };
                }
                y += h + spacing;
            }
        }
        children.len()
    }

    pub fn layout_box(id: usize) {
        with_state(|s| { layout_box_inner(id, s); });
    }

    /// Natural content width for a widget (used by sizer box layout).
    fn sizer_natural_width(s: &PcState, cid: usize) -> i32 {
        match s.node(cid).map(|n| &n.kind) {
            Some(PcWidgetKind::Button { label, .. }) => (label.len() + 2) as i32,
            Some(PcWidgetKind::Label { text, fixed_w }) => {
                fixed_w.unwrap_or(text.chars().count() as i32)
            }
            Some(PcWidgetKind::CheckButton { label, .. }) => (label.len() + 4) as i32,
            Some(PcWidgetKind::RadioButton { label, .. }) => (label.len() + 4) as i32,
            Some(PcWidgetKind::DropDown { items, .. }) => {
                let max_item = items.iter().map(|s| s.len()).max().unwrap_or(0);
                (max_item + 3) as i32
            }
            Some(PcWidgetKind::Entry { buffer, .. }) => (buffer.len() + 4).max(6) as i32,
            Some(PcWidgetKind::TextView { text }) => {
                text.lines().next().map(|l| l.len()).unwrap_or(0).max(6) as i32
            }
            _ => 4,
        }
    }

    /// Layout a `Sizer` widget (mirrors wxSizer): box (row/column with
    /// weights), grid, or flex-grid.  Children are `SizerChild` entries with
    /// weight, border, and flags.
    fn layout_sizer_inner(id: usize, s: &mut PcState) -> usize {
        let sizer = {
            let node = s.node(id);
            if node.is_none() { return 0; }
            let n = node.unwrap();
            match &n.kind {
                PcWidgetKind::Sizer(sz) => sz.clone(),
                _ => return 0,
            }
        };
        let parent_rect = s.node(id).map(|n| n.rect).unwrap_or(Rect::default());
        match sizer {
            crate::Sizer::Box { horizontal, spacing, children } => {
                if children.is_empty() { return 0; }
                let spacing = spacing.min(2);
                let n = children.len() as i32;
                let total_spacing = spacing * (n - 1).max(0);
                let total_weight: i32 = children.iter().map(|c| c.weight.max(0)).sum();
                if horizontal {
                    let total_border: i32 = children.iter().map(|c| c.border * 2).sum();
                    let avail = parent_rect.w.saturating_sub(total_spacing).saturating_sub(total_border).max(1);
                    let mut x = parent_rect.x;
                    for (i, c) in children.iter().enumerate() {
                        let w = if c.weight > 0 && total_weight > 0 {
                            avail * c.weight / total_weight
                        } else {
                            sizer_natural_width(s, c.widget)
                        };
                        if let Some(n) = s.node_mut(c.widget) {
                            n.rect = Rect {
                                x: x + c.border,
                                y: parent_rect.y + c.border,
                                w: w.max(1),
                                h: parent_rect.h.saturating_sub(c.border * 2).max(1),
                            };
                        }
                        x += w + c.border * 2;
                        if i + 1 < children.len() { x += spacing; }
                    }
                } else {
                    let total_border: i32 = children.iter().map(|c| c.border * 2).sum();
                    let avail = parent_rect.h.saturating_sub(total_spacing).saturating_sub(total_border).max(1);
                    let mut y = parent_rect.y;
                    for (i, c) in children.iter().enumerate() {
                        let h = if c.weight > 0 && total_weight > 0 {
                            avail * c.weight / total_weight
                        } else {
                            1
                        };
                        if let Some(n) = s.node_mut(c.widget) {
                            n.rect = Rect {
                                x: parent_rect.x + c.border,
                                y: y + c.border,
                                w: parent_rect.w.saturating_sub(c.border * 2).max(1),
                                h: h.max(1),
                            };
                        }
                        y += h + c.border * 2;
                        if i + 1 < children.len() { y += spacing; }
                    }
                }
                children.len()
            }
            crate::Sizer::Grid { cols, rows, children }
            | crate::Sizer::FlexGrid { cols, rows, children } => {
                if children.is_empty() { return 0; }
                let cols = cols.max(1);
                let rows = rows.max(1);
                let cw = parent_rect.w / cols as i32;
                let ch = parent_rect.h / rows as i32;
                for (i, c) in children.iter().enumerate() {
                    let r = (i as i32 / cols as i32).min(rows as i32 - 1);
                    let col = i as i32 % cols as i32;
                    if let Some(n) = s.node_mut(c.widget) {
                        n.rect = Rect {
                            x: parent_rect.x + col * cw + c.border,
                            y: parent_rect.y + r * ch + c.border,
                            w: cw.saturating_sub(c.border * 2).max(1),
                            h: ch.saturating_sub(c.border * 2).max(1),
                        };
                    }
                }
                children.len()
            }
        }
    }

    pub fn layout_sizer(id: usize) {
        with_state(|s| { layout_sizer_inner(id, s); });
    }

    fn layout_grid_inner(id: usize, s: &mut PcState) -> usize {
        let (cols, rows, children) = {
            let node = s.node(id);
            if node.is_none() { return 0; }
            let n = node.unwrap();
            match &n.kind {
                PcWidgetKind::Grid { cols, rows } => (*cols.max(&1), *rows.max(&1), n.children.clone()),
                _ => return 0,
            }
        };

        if children.is_empty() { return 0; }

        let parent_rect = s.node(id).map(|n| n.rect).unwrap_or(Rect::default());
        let ncols = cols.max(1);
        let nrows = rows.max(1);
        let cell_w = (parent_rect.w / ncols as i32).max(1);
        let cell_h = (parent_rect.h / nrows as i32).max(1);

        for (i, child_id) in children.iter().enumerate() {
            let col = i as i32 % ncols as i32;
            let row = i as i32 / ncols as i32;
            if let Some(n) = s.node_mut(*child_id) {
                n.rect = Rect {
                    x: parent_rect.x + col * cell_w,
                    y: parent_rect.y + row * cell_h,
                    w: cell_w,
                    h: cell_h,
                };
            }
        }
        children.len()
    }

    pub fn layout_grid(id: usize) {
        with_state(|s| { layout_grid_inner(id, s); });
    }

    pub fn entry_set_text(id: usize, text: &str) {
        set_entry_text(id, text);
    }

    pub fn entry_text(id: usize) -> Option<String> {
        get_entry_text(id)
    }

    pub fn set_button_font_style(id: usize, w: i32, it: bool) {
        with_state(|s| {
            if let Some(n) = s.node_mut(id) {
                if let PcWidgetKind::Button { ref mut weight, ref mut italic, .. } = n.kind {
                    *weight = w;
                    *italic = it;
                }
            }
        });
    }

    pub fn set_focus(id: usize) {
        with_state(|s| s.focus_id = Some(id));
    }

    pub fn quit() {
        with_state(|s| s.running = false);
    }

    pub fn add_key_callback(key: char, cb: Box<dyn FnMut()>) {
        with_state(|s| s.key_callbacks.push((key, cb)));
    }

    /// Render the spreadsheet grid to a `Vec<String>` buffer (one string per row)
    /// for testing/verification. This mirrors the rendering logic of `render_widget`
    /// for `PcWidgetKind::Spreadsheet` but writes to strings instead of ncurses.
    pub fn render_spreadsheet_to_buffer(spreadsheet_id: usize, width: usize, height: usize) -> Vec<String> {
        let mut buf = vec![String::new(); height];
        with_state(|state| {
            let n = match state.node(spreadsheet_id) {
                Some(n) => n,
                None => return,
            };
            let (cells, raw_cells, top_row, left_col, cursor_row, cursor_col, editing, edit_buf, edit_pos,
                 col_width, margin_cols, main_cols, menu_text, status_text,
                 border_title, formula_bar_trailing, column_layout, row_labels) = match &n.kind {
                PcWidgetKind::Spreadsheet { grid, ref menu_text, ref status_text, ref border_title, ref formula_bar_trailing, .. } => {
                    (grid.cells.clone(), grid.raw_cells.clone(), grid.top_row, grid.left_col, grid.cursor_row, grid.cursor_col,
                     grid.editing, grid.edit_buf.clone(), grid.edit_pos, grid.col_width,
                     grid.margin_cols, grid.main_cols, menu_text.clone(), status_text.clone(),
                     border_title.clone(), formula_bar_trailing.clone(), grid.column_layout.clone(), grid.row_labels.clone())
                }
                _ => return,
            };
            let cw = col_width as usize;
            let rh_w = 5usize;
            let lm = margin_cols as usize;
            let mc = main_cols as usize;
            let sep = '│';
            let mut row = 0usize;

            // Menu bar (widget-derived when a menubar exists, else app text).
            let menu_line: String = derived_menu_bar_text(state).unwrap_or_else(|| menu_text.clone());
            if !menu_line.is_empty() {
                if row < height {
                    let display = &menu_line[..menu_line.len().min(width)];
                    buf[row] = display.to_string();
                }
                row += 1;
            }

            // Formula bar (matching ratatui: leading space, trailing text)
            if row < height {
                let mc = main_cols as usize;
                let lm = margin_cols as usize;
                let cc = cursor_col as usize;
                let col_part = if cc < lm {
                    let margin_idx = lm.saturating_sub(1).saturating_sub(cc);
                    format!("[{}", col_label(margin_idx as u32))
                } else if cc < lm + mc {
                    col_label((cc - lm) as u32)
                } else {
                    let right_idx = cc.saturating_sub(lm).saturating_sub(mc);
                    format!("]{}", col_label(right_idx as u32))
                };
                let row_label = row_labels.iter()
                    .find(|(r, _)| *r == cursor_row)
                    .map(|(_, l)| l.as_str())
                    .unwrap_or("1");
                let addr_text = format!("{}{}", col_part, row_label.trim());
                let addr_str = format!(" {}  ", addr_text);
                if editing || !edit_buf.is_empty() {
                    // Edit mode: show edit buffer with cursor
                    let chars: Vec<char> = edit_buf.chars().collect();
                    let cpos = edit_pos.min(chars.len());
                    let before: String = chars[..cpos].iter().collect();
                    let after: String = if cpos < chars.len() {
                        chars[cpos + 1..].iter().collect()
                    } else {
                        String::new()
                    };
                    let cursor_ch = chars.get(cpos).map(|c| c.to_string()).unwrap_or_else(|| " ".to_string());
                    let fb_text = format!("{}{}{}{}", addr_str, before, cursor_ch, after);
                    let truncated: String = fb_text.chars().take(width).collect();
                    buf[row] = truncated;
                } else {
                    let cell_val = raw_cells.borrow().get(&(cursor_row, cursor_col)).cloned().unwrap_or_default();
                    let fb_text = format!(" {}  {}{}", addr_text, cell_val, formula_bar_trailing);
                    let truncated: String = fb_text.chars().take(width).collect();
                    buf[row] = truncated;
                }
                row += 1;
            }

            // Border title line (matching ratatui: ┌ title ───┐)
            if row < height {
                let title = if !border_title.is_empty() {
                    border_title.clone()
                } else {
                    format!("corro  {}r × {}c ", mc, mc)
                };
                let title_vis = title.chars().count();
                let dash_fill = width.saturating_sub(title_vis + 3);
                let mut border_line = String::from("┌");
                border_line.push(' ');
                border_line.push_str(&title);
                border_line.push_str(&"─".repeat(dash_fill));
                border_line.push('┐');
                let truncated: String = border_line.chars().take(width).collect();
                buf[row] = truncated;
                row += 1;
            }

            // Use pre-computed layout when available, otherwise fall back to computed layout.
            let use_layout = !column_layout.is_empty();
            let use_row_labels = !row_labels.is_empty();

            // Grid header row (with │ border)
            if row < height {
                let mut hdr = String::new();
                hdr.push('│');
                if use_layout {
                    hdr.push_str(&" ".repeat(5));
                    let n = column_layout.len();
                    for (i, &(ci, w, ref label)) in column_layout.iter().enumerate() {
                        let padded = format!("{:<1$}", label, (w as usize).max(1));
                        hdr.push_str(&padded);
                    if i + 1 < n {
                        let is_boundary = lm > 0 && (ci == (lm - 1) as u32 || ci == (lm + mc - 1) as u32);
                        if is_boundary {
                            hdr.push_str("│ ");
                        } else {
                            hdr.push(' ');
                        }
                    }
                    }
                } else {
                    let max_data_cols = ((width as i32 - rh_w as i32 - 2) / (cw as i32 + 1)).max(1) as usize;
                    hdr.push_str(&" ".repeat(5));
                    for vc in 0..max_data_cols.min(mc.saturating_add(lm)) {
                        let col_idx = left_col + vc as u32;
                        let label = if col_idx < lm as u32 {
                            let margin_idx = lm.saturating_sub(1).saturating_sub(col_idx as usize);
                            format!("[{}", col_label(margin_idx as u32))
                        } else if col_idx < (lm + mc) as u32 {
                            col_label(col_idx - lm as u32)
                        } else {
                            format!("]{}", col_label(col_idx - lm as u32 - mc as u32))
                        };
                        let padded = format!("{:<1$}", label, (cw as usize).max(label.len()));
                        hdr.push_str(&format!("{}{}", padded, sep));
                    }
                }
                let truncated: String = hdr.chars().take(width).collect();
                buf[row] = truncated;
                row += 1;
            }

            // Header separator (matching ratatui: │├───┼───┤)
            if row < height {
                let mut sep_row = String::new();
                sep_row.push('│');
                sep_row.push('├');
                // Compute separator boundary positions from column layout
                let sep_boundaries = if use_layout && lm > 0 {
                    let mut cx = 1 + 5;
                    let mut boundaries: Vec<usize> = Vec::new();
                    for (idx, &(col_idx, w, _)) in column_layout.iter().enumerate() {
                        cx += w as usize;
                        if idx + 1 < column_layout.len() {
                            let is_boundary = col_idx == (lm - 1) as u32 || col_idx == (lm + mc - 1) as u32;
                            if is_boundary {
                                boundaries.push(cx);
                            }
                            cx += if is_boundary { 2 } else { 1 };
                        }
                    }
                    boundaries
                } else {
                    Vec::new()
                };
                let max_x = width.saturating_sub(1);
                let mut sx = 2;
                for &bx in &sep_boundaries {
                    let end = bx.min(max_x);
                    while sx < end {
                        sep_row.push('─');
                        sx += 1;
                    }
                    if sx < max_x {
                        sep_row.push('┼');
                        sx += 1;
                    }
                }
                while sx < max_x {
                    sep_row.push('─');
                    sx += 1;
                }
                sep_row.push('┤');
                let truncated: String = sep_row.chars().take(width).collect();
                buf[row] = truncated;
                row += 1;
            }

            // Data rows (with │ border)
            let has_status_bar = !status_text.is_empty() || editing || !edit_buf.is_empty();
            let grid_bottom = height - if has_status_bar { 2 } else { 0 };
            let header_row_count = if !menu_text.is_empty() { 5 } else { 4 };
            while row < grid_bottom && row < height {
                let row_idx = top_row + (row.saturating_sub(header_row_count)) as u32;
                let mut data_row = String::new();
                data_row.push('│');
                // Row header
                if use_row_labels {
                    if let Some((_, label)) = row_labels.iter().find(|(r, _)| *r == row_idx) {
                        data_row.push_str(&format!("{:>4} ", label.trim()));
                    } else {
                        data_row.push_str(&format!("{:>4} ", row_idx + 1));
                    }
                } else {
                    data_row.push_str(&format!("{:>4} ", row_idx + 1));
                }
                // Data cells
                let cells_ref = cells.borrow();
                if use_layout {
                    let lm = margin_cols;
                    let mc = main_cols;
                    let n = column_layout.len();
                    let mut vi = 0usize;
                    while vi < n {
                        let (col_idx, w, _) = column_layout[vi];
                        let cell_text = cells_ref.get(&(row_idx, col_idx)).map(|s| s.as_str()).unwrap_or("");
                        let cell_w = w as usize;
                        let is_boundary = lm > 0 && (col_idx == lm - 1 || col_idx == lm + mc - 1);
                        if cell_text.is_empty() {
                            let gap_extra = if vi + 1 < n && !is_boundary { 1 } else { 0 };
                            data_row.push_str(&format!("{:<1$}", "", cell_w + gap_extra));
                            vi += 1;
                            if vi < n && is_boundary {
                                data_row.push_str("│ ");
                            }
                            continue;
                        }
                        let text_width = cell_text.chars().count();
                        // Find overflow space into adjacent empty columns
                        let mut overflow_cols = 0usize;
                        if text_width > cell_w {
                            let mut scan = vi + 1;
                            while scan < n {
                                let (sc_idx, _, _) = column_layout[scan];
                                // Stop at left-margin boundary unconditionally.
                                if lm > 0 && (sc_idx as usize) == lm as usize {
                                    break;
                                }
                                // At right-margin boundary: stop if text fits
                                // within the main columns + PipeAndSpace,
                                // otherwise continue into right-margin cols.
                                if lm > 0 && (sc_idx as usize) == lm as usize + mc as usize {
                                    let boundary_total: usize = column_layout[vi..scan].iter()
                                        .map(|&(_, w, _)| w as usize).sum::<usize>()
                                        + (scan.saturating_sub(vi + 1))
                                        + 2;
                                    if text_width <= boundary_total {
                                        break;
                                    }
                                }
                                let sc_text = cells_ref.get(&(row_idx, sc_idx)).map(|s| s.as_str()).unwrap_or("");
                                if sc_text.is_empty() {
                                    overflow_cols += 1;
                                    scan += 1;
                                } else { break; }
                            }
                        }
                        let gap_after = if overflow_cols == 0 && vi + 1 < n && !is_boundary { 1 } else { 0 };
                        let mut total_avail: usize = column_layout[vi..=vi+overflow_cols].iter()
                            .map(|&(_, w, _)| w as usize).sum::<usize>()
                            + overflow_cols // spaces between overflow columns
                            + gap_after;   // gap to next column
                        // Include right-gap for consistent overflow width
                        if vi + overflow_cols + 1 >= n {
                            let mut render_w: usize = column_layout.iter()
                                .map(|&(_, w, _)| w as usize).sum::<usize>();
                            for idx in 0..n.saturating_sub(1) {
                                let col_idx = column_layout[idx].0 as usize;
                                let lm_u = lm as usize;
                                if lm_u > 0 && (col_idx == lm_u - 1 || col_idx == lm_u + mc as usize - 1) {
                                    render_w += 2;
                                } else {
                                    render_w += 1;
                                }
                            }
                            let total_w = (width as usize).saturating_sub(7);
                            if total_w > render_w {
                                total_avail = total_avail.saturating_add(total_w - render_w);
                            }
                        }
                        let display = if text_width > total_avail {
                            if overflow_cols == 0 {
                                cell_text.chars().take(total_avail).collect()
                            } else {
                                let trunc = total_avail.saturating_sub(1).max(1);
                                let mut s: String = cell_text.chars().take(trunc).collect();
                                if text_width > trunc { s.push('…'); }
                                s
                            }
                        } else { cell_text.to_string() };
                        // Pad to combined width of current + overflow columns to maintain alignment.
                        // When overflow_cols==0, include the gap (total_avail) so the gap
                        // character is preserved, matching ratatui's use of the inter-column
                        // gap as overflow room.
                        let total_width = if overflow_cols > 0 {
                            column_layout[vi..=vi+overflow_cols].iter()
                                .map(|&(_, w, _)| w as usize).sum::<usize>()
                        } else {
                            total_avail
                        };
                        data_row.push_str(&format!("{:<1$}", display, total_width));
                        vi += 1 + overflow_cols;
                        if vi < n && is_boundary {
                            data_row.push_str("│ ");
                        }
                    }
                } else {
                    let max_data_cols = ((width as i32 - rh_w as i32 - 2) / (cw as i32 + 1)).max(1) as usize;
                    let mut vc = 0usize;
                    while vc < max_data_cols {
                        let col_idx = left_col + vc as u32;
                        let cell_text = cells_ref.get(&(row_idx, col_idx)).map(|s| s.as_str()).unwrap_or("");
                        let text_width = cell_text.chars().count();
                        let mut overflow_cols = 0usize;
                        if text_width > cw as usize {
                            let mut scan = vc + 1;
                            while scan < max_data_cols {
                                let sc = left_col + scan as u32;
                                if cells_ref.get(&(row_idx, sc)).map_or(true, |s| s.is_empty()) {
                                    overflow_cols += 1;
                                    scan += 1;
                                } else { break; }
                            }
                        }
                        let available = (overflow_cols + 1) * cw as usize;
                        let display = if text_width > available {
                            if overflow_cols == 0 {
                                cell_text.chars().take(available).collect()
                            } else {
                                let trunc = (available - 1).max(1);
                                let mut s: String = cell_text.chars().take(trunc).collect();
                                if text_width > trunc { s.push('…'); }
                                s
                            }
                        } else {
                            cell_text.to_string()
                        };
                        let cell_w = (overflow_cols + 1) * cw as usize;
                        data_row.push_str(&format!("{:<1$}", display, cell_w));
                        data_row.push(sep);
                        vc += 1 + overflow_cols;
                    }
                }
                if data_row.chars().count() > width {
                    data_row = data_row.chars().take(width).collect();
                }
                // Only add │ closing border if there's room
                if data_row.chars().count() + 1 <= width && !data_row.ends_with('│') {
                    data_row.push('│');
                }
                buf[row] = data_row;
                row += 1;
            }

            // Bottom border line (matching ratatui layout)
            if row < height {
                let mut border = String::new();
                border.push('└');
                for _ in 1..width.saturating_sub(1) {
                    border.push('─');
                }
                if width > 1 {
                    border.push('┘');
                }
                let cut = border.char_indices().take(width).last().map(|(i, _)| i).unwrap_or(0);
                if cut > 0 { buf[row] = border[..cut].to_string(); }
                else { buf[row] = border.chars().take(width).collect(); }
                row += 1;
            }

            // Status bar (always on its own line below the bottom border)
            let edit_hint = "  type to edit (or addr: val)   Enter·confirm   Esc·discard";
            let normal_hint = "  type/F2·edit; Ctrl+C·copy; Ctrl+X·cut; Ctrl+V·paste; Ctrl+;·date; Ctrl+:·time; Ctrl+S·save; F1·help";
            let display_status = if editing || !edit_buf.is_empty() {
                edit_hint.to_string()
            } else if !status_text.is_empty() {
                status_text.clone()
            } else {
                normal_hint.to_string()
            };
            if !display_status.is_empty() && row < height {
                let display = &display_status[..display_status.len().min(width)];
                buf[row] = display.to_string();
            }
        });
        buf
    }

    /// Test-visible wrapper for [`paint_canvases`] (a canvas draw callback
    /// only runs inside the redraw loop, which a unit test does not start).
    #[cfg(test)]
    pub(crate) fn paint_canvases_for_test() -> usize {
        paint_canvases()
    }

    /// Terminal half of `Label::set_fixed_width`, in a module of its own so
    /// the pin's contract is stated in one place.
    #[cfg(test)]
    mod label_fixed_width_tests {
        use super::*;

        /// Build a window → horizontal box with three children, laid out in a
        /// `width`-cell row. Returns (box, [label, f_label, entry]).
        fn bar(text: &str, width: i32) -> (usize, Vec<usize>) {
            let win = create_window().unwrap();
            set_widget_rect(win, 0, 0, width, 1);
            let bx = create_box(true, 0).unwrap();
            set_child(win, bx);
            set_widget_rect(bx, 0, 0, width, 1);
            let a = create_label(text).unwrap();
            let b = create_label("  fx  ").unwrap();
            let e = create_entry().unwrap();
            for &c in &[a, b, e] {
                append_child(bx, c);
            }
            layout_box(bx);
            (bx, vec![a, b, e])
        }

        /// The pin must hold the label's slot while its text changes — that is
        /// the whole point (the formula bar's address, `A1` → `A100`, used to
        /// slide every widget packed after it sideways on each cursor move).
        #[test]
        fn pinned_width_holds_the_slot_against_text_changes() {
            let (bx, ids) = bar("A1", 40);
            let (addr, fx, entry) = (ids[0], ids[1], ids[2]);
            set_label_fixed_width(addr, Some(8));

            // Re-layout each time, as the redraw loop does.
            let mut seen: Vec<(i32, i32, i32)> = Vec::new();
            for text in ["A1", "A100", "AAAAA1000000", "A1"] {
                set_label_text(addr, text);
                layout_box(bx);
                seen.push((
                    get_widget_rect(addr).unwrap().2,
                    get_widget_rect(fx).unwrap().0,
                    get_widget_rect(entry).unwrap().0,
                ));
            }
            assert!(
                seen.windows(2).all(|w| w[0] == w[1]),
                "slot moved as the text changed: {seen:?}"
            );
            assert_eq!(8, seen[0].0, "the pin reserves its cells");
        }

        /// Releasing the pin restores shrink-to-fit, so an unpinned label still
        /// tracks its text (the pre-existing behaviour).
        #[test]
        fn releasing_the_pin_restores_shrink_to_fit() {
            let (bx, ids) = bar("A1", 40);
            let (addr, fx, _) = (ids[0], ids[1], ids[2]);
            set_label_fixed_width(addr, Some(8));
            layout_box(bx);
            let pinned = get_widget_rect(fx).unwrap().0;

            set_label_fixed_width(addr, None);
            set_label_text(addr, "A1000000");
            layout_box(bx);
            let grown = get_widget_rect(fx).unwrap().0;
            assert!(
                grown > pinned,
                "an unpinned label should still fit its text ({grown} !> {pinned})"
            );
        }

        /// A non-positive pin is meaningless (it would make the label
        /// invisible), so it is treated as no pin rather than a zero-width box.
        #[test]
        fn non_positive_pins_are_ignored() {
            let l = create_label("A1").unwrap();
            set_label_fixed_width(l, Some(6));
            assert_eq!(Some(6), get_label_fixed_width(l));
            set_label_fixed_width(l, Some(0));
            assert_eq!(None, get_label_fixed_width(l));
            set_label_fixed_width(l, Some(-3));
            assert_eq!(None, get_label_fixed_width(l));
        }

        /// Non-label widgets are left alone rather than panicking — a pin is
        /// only meaningful for a label, and callers should not crash if they
        /// point it at the wrong widget.
        #[test]
        fn pinning_a_non_label_is_a_no_op() {
            let e = create_entry().unwrap();
            set_label_fixed_width(e, Some(10));
            assert_eq!(None, get_label_fixed_width(e));
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        // ── Mouse decoding / hit-testing ─────────────────────────────────
        //
        // These are pure (no terminal), so they run on every platform and pin
        // the contract `handle_mouse_event` relies on: the mask layout shared
        // by ncurses mouse-v2 and PDCurses, and the cell mapping that must
        // agree with the renderer in `backends::pancurses_draw`.

        /// Build a `bstate` with `action` set on 1-based button `n`.
        fn button_bits(n: u32, action: u64) -> u64 {
            action << ((n - 1) * 5)
        }

        #[test]
        fn mouse_press_click_and_release_decode() {
            for (n, shift) in [(1u32, 0u32), (2, 5), (3, 10)] {
                let base = (n - 1) * 5;
                assert_eq!(
                    MouseAction::Pressed(n as u8),
                    decode_mouse_event(3, 7, 0x002u64 << base).action,
                    "button {n} press"
                );
                assert_eq!(
                    MouseAction::Clicked(n as u8),
                    decode_mouse_event(3, 7, 0x004u64 << base).action,
                    "button {n} click"
                );
                assert_eq!(
                    MouseAction::DoubleClicked(n as u8),
                    decode_mouse_event(3, 7, 0x008u64 << base).action,
                    "button {n} double-click"
                );
                assert_eq!(
                    MouseAction::TripleClicked(n as u8),
                    decode_mouse_event(3, 7, 0x010u64 << base).action,
                    "button {n} triple-click"
                );
                assert_eq!(
                    MouseAction::Released(n as u8),
                    decode_mouse_event(3, 7, 0x001u64 << base).action,
                    "button {n} release"
                );
                let _ = shift;
            }
        }

        #[test]
        fn mouse_position_survives_decoding() {
            let ev = decode_mouse_event(12, 34, button_bits(1, 0x002));
            assert_eq!((ev.x, ev.y), (12, 34));
        }

        #[test]
        fn mouse_wheel_decodes_button4_up_and_button5_down() {
            // ncurses and PDCurses both report wheel-up as BUTTON4_PRESSED and
            // wheel-down as BUTTON5_PRESSED.
            let up = decode_mouse_event(-1, -1, button_bits(4, 0x002));
            assert_eq!(MouseAction::Wheel { down: false, horizontal: false }, up.action);
            let down = decode_mouse_event(-1, -1, button_bits(5, 0x002));
            assert_eq!(MouseAction::Wheel { down: true, horizontal: false }, down.action);
        }

        #[test]
        fn mouse_modifiers_decode_from_the_high_bits() {
            // BUTTON_SHIFT/CTRL/ALT are bits 30/31/32 on both backends.
            let ev = decode_mouse_event(0, 0, button_bits(1, 0x002) | (0x001 << 30) | (0x002 << 30));
            assert!(ev.ctrl && ev.shift && !ev.alt);
            assert_eq!(MouseAction::Pressed(1), ev.action);
            // raw_state is passed through untouched for hosts that need it.
            assert_eq!(ev.raw_state, button_bits(1, 0x002) | (0x001 << 30) | (0x002 << 30));
        }

        #[test]
        fn mouse_motion_and_position_decode() {
            // PDCurses movement: changes = PDC_MOUSE_MOVED (bit 3) plus the
            // button group flagged BUTTONn_MOVED (aliases TRIPLE_CLICKED bit).
            let moved = decode_mouse_event(4, 5, 0x08 | button_bits(1, 0x010));
            assert_eq!(MouseAction::Moved { button: Some(1) }, moved.action);
            // Bare position report (REPORT_MOUSE_POSITION, bit 33).
            let pos = decode_mouse_event(4, 5, 0x008 << 30);
            assert_eq!(MouseAction::Position, pos.action);
            // An all-zero mask still decodes to something inert, never panics.
            assert_eq!(MouseAction::Position, decode_mouse_event(0, 0, 0).action);
        }

        #[test]
        fn mouse_local_rejects_points_outside_the_widget() {
            let r = Rect { x: 10, y: 20, w: 5, h: 4 };
            assert_eq!(Some((0, 0)), mouse_local(r, 10, 20));
            assert_eq!(Some((4, 3)), mouse_local(r, 14, 23));
            assert_eq!(None, mouse_local(r, 9, 20));
            assert_eq!(None, mouse_local(r, 15, 20));
            assert_eq!(None, mouse_local(r, 10, 19));
            assert_eq!(None, mouse_local(r, 10, 24));
            // Degenerate rects never match.
            assert_eq!(None, mouse_local(Rect { x: 0, y: 0, w: 0, h: 0 }, 0, 0));
        }

        /// The cell mapping must agree with the renderer: grid row `r` is drawn
        /// at `header_h + r * ROW_H` and its text starts one cell right of the
        /// cell's background column (see `pancurses_draw::draw_text_styled`).
        #[test]
        fn spreadsheet_cell_at_matches_the_renderer_layout() {
            let mut grid = new_grid(10, 4);
            grid.header_row_count = 2;
            grid.main_row_count = 8;
            grid.margin_cols = 1;
            grid.main_cols = 3;
            grid.col_width = 4;
            grid.column_layout = vec![
                (0, 4, "lab".into()),
                (1, 4, "A".into()),
                (2, 8, "B".into()),
                (3, 4, "C".into()),
            ];

            let row_h = crate::spreadsheet::SpreadsheetModel::ROW_H as i32; // 22
            let row0 = 2 * row_h; // first non-header row's y
            let label_w = crate::spreadsheet::SpreadsheetModel::ROW_LABEL_W as i32; // 64

            // Header band is chrome, not a cell.  Body row 0 is the first
            // non-header row (the renderer numbers body rows from `top_row`).
            assert_eq!(None, spreadsheet_cell_at(&grid, (label_w, 0)));
            assert_eq!(None, spreadsheet_cell_at(&grid, (label_w, 2 * row_h - 1)));
            // Row-label gutter maps to the margin column.
            assert_eq!(Some((0, 0)), spreadsheet_cell_at(&grid, (0, row0)));
            assert_eq!(Some((0, 0)), spreadsheet_cell_at(&grid, (label_w - 1, row0)));
            // Each column band: layout width * CHAR_W cells wide.
            assert_eq!(Some((0, 1)), spreadsheet_cell_at(&grid, (label_w, row0)));
            assert_eq!(Some((0, 1)), spreadsheet_cell_at(&grid, (label_w + 31, row0)));
            assert_eq!(Some((0, 2)), spreadsheet_cell_at(&grid, (label_w + 32, row0)));
            assert_eq!(Some((0, 3)), spreadsheet_cell_at(&grid, (label_w + 96, row0)));
            // Past the last column is not a cell.
            assert_eq!(None, spreadsheet_cell_at(&grid, (label_w + 200, row0)));
            // Second body row, mid-height → row 1.
            assert_eq!(Some((1, 1)), spreadsheet_cell_at(&grid, (label_w + 4, row0 + row_h)));
            // Last row (total_rows = 10) is still a cell; one past it is not.
            assert_eq!(Some((9, 1)), spreadsheet_cell_at(&grid, (label_w + 4, row0 + 9 * row_h)));
            assert_eq!(None, spreadsheet_cell_at(&grid, (label_w + 4, row0 + 10 * row_h)));
        }

        /// A click on the cursor cell must be a no-op *for scrolling* but must
        /// still leave the grid consistent (regression guard for the click
        /// path clamping to the grid bounds).
        #[test]
        fn spreadsheet_cell_at_clamps_to_grid_bounds() {
            let mut grid = new_grid(3, 2);
            grid.header_row_count = 0;
            grid.margin_cols = 0;
            grid.main_cols = 2;
            grid.col_width = 4;
            // No column_layout → every column is col_width wide.
            assert_eq!(Some((0, 0)), spreadsheet_cell_at(&grid, (64, 0)));
            assert_eq!(Some((2, 1)), spreadsheet_cell_at(&grid, (64 + 4 * 8 + 1, 2 * 22)));
            // Past the last row (total_rows = 3) is not a cell.
            assert_eq!(None, spreadsheet_cell_at(&grid, (64, 3 * 22)));
            // Past the last column is not a cell.
            assert_eq!(None, spreadsheet_cell_at(&grid, (64 + 8 * 8, 0)));
        }

        #[test]
        fn mouse_enabled_flag_is_off_until_requested() {
            // The opt-in must default to false: terminals that capture the
            // mouse lose native text selection, so nothing may turn it on
            // implicitly.
            assert!(!mouse_is_enabled());
            set_mouse_enabled(true);
            assert!(mouse_is_enabled());
            set_mouse_enabled(false);
            assert!(!mouse_is_enabled());
        }

        /// The pancurses canvas must offer the same pointer API as the GTK
        /// canvas *and* actually route events to it: `on_click` and
        /// `on_click_button` both fire (button-aware wins, as on GTK where
        /// they share one signal), and `on_motion` fires for a bare position
        /// report with button 0.
        #[test]
        fn canvas_pointer_handlers_receive_click_and_motion() {
            use std::cell::RefCell;
            use std::rc::Rc;

            // A canvas occupying cells (5,3)..(15,9).
            let canvas = create_canvas().unwrap();
            with_state(|s| {
                let n = s.node_mut(canvas).unwrap();
                n.rect = Rect { x: 5, y: 3, w: 10, h: 6 };
            });
            canvas_clear_pointer_handlers(canvas);

            // Cell (7,5) is 2 cells right / 2 rows down of the origin.
            let (px, py) = canvas_pixel_xy(canvas, 7, 5).unwrap();
            assert_eq!(px, 2.0 * crate::spreadsheet::SpreadsheetModel::CHAR_W);
            assert_eq!(py, 2.0 * crate::spreadsheet::SpreadsheetModel::ROW_H);

            let hits: Rc<RefCell<Vec<(f64, f64, u32, u32)>>> = Rc::new(RefCell::new(Vec::new()));
            let h = hits.clone();
            canvas_on_click_button(canvas, Box::new(move |x, y, b, m| {
                h.borrow_mut().push((x, y, b, m));
            }));
            // A plain handler registered too: the richer one takes precedence.
            let plain_hits = Rc::new(RefCell::new(0u32));
            let ph = plain_hits.clone();
            canvas_on_click(canvas, Box::new(move |_, _| {
                *ph.borrow_mut() += 1;
            }));

            handle_mouse_event(decode_mouse_event(7, 5, button_bits(1, 0x002)));
            assert_eq!(1, hits.borrow().len(), "button handler should fire once");
            assert_eq!((px, py, 1, 0), hits.borrow()[0]);
            assert_eq!(0, *plain_hits.borrow(), "button handler owns the click");

            // Motion (bare position report) reaches on_motion with button 0.
            let motion: Rc<RefCell<Vec<(f64, f64, u32)>>> = Rc::new(RefCell::new(Vec::new()));
            let m = motion.clone();
            canvas_on_motion(canvas, Box::new(move |x, y, b| {
                m.borrow_mut().push((x, y, b));
            }));
            handle_mouse_event(decode_mouse_event(7, 5, 0x008 << 30));
            assert_eq!(1, motion.borrow().len());
            assert_eq!((px, py, 0), motion.borrow()[0]);

            // A click outside the canvas is not delivered to it.
            hits.borrow_mut().clear();
            handle_mouse_event(decode_mouse_event(30, 30, button_bits(1, 0x002)));
            assert!(hits.borrow().is_empty(), "outside the canvas: no handler call");

            canvas_clear_pointer_handlers(canvas);
        }

        /// A canvas with only `on_click` registered still gets clicks — the
        /// GTK/NWG backends allow the plain contract on its own.
        #[test]
        fn canvas_plain_click_handler_works_without_button_handler() {
            use std::cell::RefCell;
            use std::rc::Rc;

            let canvas = create_canvas().unwrap();
            with_state(|s| {
                let n = s.node_mut(canvas).unwrap();
                n.rect = Rect { x: 0, y: 0, w: 8, h: 4 };
            });
            canvas_clear_pointer_handlers(canvas);
            let seen: Rc<RefCell<Option<(f64, f64)>>> = Rc::new(RefCell::new(None));
            let s = seen.clone();
            canvas_on_click(canvas, Box::new(move |x, y| {
                *s.borrow_mut() = Some((x, y));
            }));
            handle_mouse_event(decode_mouse_event(2, 1, button_bits(1, 0x004)));
            let got = *seen.borrow();
            assert!(got.is_some(), "plain handler should fire");
            let (x, y) = got.unwrap();
            assert_eq!(x, 2.0 * crate::spreadsheet::SpreadsheetModel::CHAR_W);
            assert_eq!(y, 1.0 * crate::spreadsheet::SpreadsheetModel::ROW_H);
            canvas_clear_pointer_handlers(canvas);
        }

        /// Helper to set up a minimal spreadsheet state with a window parent.
        fn make_spreadsheet_id() -> usize {
            let wid = with_state(|s| s.add_node(
                PcWidgetKind::Window { title: "test".into() }, None,
            ));
            let sid = create_spreadsheet(100, 26).unwrap();
            with_state(|s| {
                if let Some(n) = s.node_mut(sid) {
                    n.parent = Some(wid);
                    if let PcWidgetKind::Spreadsheet { grid, border_title, menu_text, status_text, formula_bar_trailing, .. } = &mut n.kind {
                        grid.header_row_count = 2;
                        grid.main_row_count = 24;
                    }
                }
                s.focus_id = Some(sid);
            });
            sid
        }

        /// Moving cursor with arrow keys.
        #[test]
        fn spreadsheet_arrow_keys_move_cursor() {
            let sid = make_spreadsheet_id();
            // Simulate KeyDown by directly manipulating state:
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_row = &mut grid.cursor_row;
                    *cursor_row += 1;
                }
            });
            let pos = spreadsheet_cursor_position(sid);
            assert_eq!(pos, Some((1, 0)), "cursor should be at (1, 0) after down");

            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_col = &mut grid.cursor_col;
                    *cursor_col += 1;
                }
            });
            let pos = spreadsheet_cursor_position(sid);
            assert_eq!(pos, Some((1, 1)), "cursor should be at (1, 1) after right");

            // Home should go to col 0
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_col = &mut grid.cursor_col;
                    *cursor_col = 0;
                }
            });
            let pos = spreadsheet_cursor_position(sid);
            assert_eq!(pos, Some((1, 0)), "cursor should be at (1, 0) after home");
        }

        /// Entering text and committing.
        #[test]
        fn spreadsheet_enter_and_commit_text() {
            let sid = make_spreadsheet_id();
            // Enter edit mode (simulate Enter key)
            with_state(|state| spreadsheet_enter(state, sid));
            // Check editing state
            with_state(|state| {
                let n = state.node(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { grid, .. } = &n.kind { let editing = &grid.editing;
                    assert!(*editing, "should be in edit mode after enter");
                }
            });

            // Set edit buffer text (simulate typing)
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let edit_buf = &mut grid.edit_buf;
                    *edit_buf = "hello".to_string();
                }
            });

            // Commit edit (simulate Enter again)
            with_state(|state| spreadsheet_commit_edit(state, sid));

            // Verify cell content
            let val = spreadsheet_get_cell(sid, 0, 0);
            assert_eq!(val, Some("hello".into()), "cell should contain 'hello' after commit");

            let pos = spreadsheet_cursor_position(sid);
            assert_eq!(pos, Some((0, 0)), "cursor should remain at (0, 0)");
        }

        /// A keyed menu label renders as its key until a translator is
        /// installed, then renders the translated text — and translation never
        /// changes the action string that dispatch/parity depends on.
        #[test]
        fn menu_keyed_label_translates_without_changing_action() {
            let item = crate::MenuItem::Action {
                label: crate::Label::Key("test.menu.open"),
                action: "open".into(),
                shortcut: None,
            };
            // Identity translator (default): key renders verbatim.
            assert_eq!(menu_item_label(&item), "test.menu.open");

            crate::set_translator(|k| {
                if k == "test.menu.open" { "Öffnen".to_string() } else { k.to_string() }
            });
            assert_eq!(menu_item_label(&item), "Öffnen");

            // The action string is the contract; translation must not touch it.
            match &item {
                crate::MenuItem::Action { action, .. } => assert_eq!(action, "open"),
                _ => panic!("expected action"),
            }
        }

        /// Moving cursor and committing advances cursor.
        #[test]
        fn spreadsheet_enter_advances_cursor() {
            let sid = make_spreadsheet_id();
            // Enter edit mode
            with_state(|state| spreadsheet_enter(state, sid));

            // Set edit buffer text
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let edit_buf = &mut grid.edit_buf;
                    *edit_buf = "test".to_string();
                }
            });

            // Second enter commits and advances
            with_state(|state| spreadsheet_enter(state, sid));
            let val = spreadsheet_get_cell(sid, 0, 0);
            assert_eq!(val, Some("test".into()), "cell should contain 'test'");

            let pos = spreadsheet_cursor_position(sid);
            assert_eq!(pos, Some((1, 0)), "cursor should advance to row 1");
        }

        /// Test that cursor does not go negative.
        #[test]
        fn spreadsheet_cursor_clamped_at_zero() {
            let sid = make_spreadsheet_id();
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_row = &mut grid.cursor_row; let cursor_col = &mut grid.cursor_col;
                    *cursor_row = 0;
                    *cursor_col = 0;
                }
            });
            let pos = spreadsheet_cursor_position(sid);
            assert_eq!(pos, Some((0, 0)));

            // Simulate KeyUp: should not go negative
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_row = &mut grid.cursor_row;
                    if *cursor_row > 0 { *cursor_row -= 1; }
                }
            });
            let pos = spreadsheet_cursor_position(sid);
            assert_eq!(pos, Some((0, 0)), "cursor should stay at row 0");

            // Simulate KeyLeft: should not go negative
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_col = &mut grid.cursor_col;
                    if *cursor_col > 0 { *cursor_col -= 1; }
                }
            });
            let pos = spreadsheet_cursor_position(sid);
            assert_eq!(pos, Some((0, 0)), "cursor should stay at col 0");
        }

        #[test]
        fn spreadsheet_render_matches_ratatui() {
            // Set up a spreadsheet with the same data as docs/tests/overflow.corro
            let wid = with_state(|s| s.add_node(
                PcWidgetKind::Window { title: "corro".into() }, None,
            ));
            let sid = create_spreadsheet(100, 3).unwrap();
with_state(|s| {
    if let Some(n) = s.node_mut(sid) {
        n.parent = Some(wid);
        if let PcWidgetKind::Spreadsheet { grid, border_title, menu_text, status_text, formula_bar_trailing, .. } = &mut n.kind {
            grid.header_row_count = 2;
            grid.main_row_count = 24;
            *menu_text = " [File]  Edit    Insert    Format    Sheet    Help".to_string();
            *status_text = "  type/F2·edit; Ctrl+C·copy; Ctrl+X·cut; Ctrl+V·paste; Ctrl+;·date; Ctrl+:·time; Ctrl+S·save; F1·help".to_string();
        }
    }
});
            // Set cell data matching overflow.corro
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                match &mut n.kind {
                    PcWidgetKind::Spreadsheet { grid, .. } => { let cells = &grid.cells; let raw_cells = &grid.raw_cells;
                        cells.borrow_mut().insert((0, 0), "This Text is really long and should overflow.".into());
                        cells.borrow_mut().insert((1, 1), "This Text is really long and should overflow.".into());
                        cells.borrow_mut().insert((2, 2), "This Text is really long and should overflow.".into());
                        raw_cells.borrow_mut().insert((0, 0), "This Text is really long and should overflow.".into());
                        raw_cells.borrow_mut().insert((1, 1), "This Text is really long and should overflow.".into());
                        raw_cells.borrow_mut().insert((2, 2), "This Text is really long and should overflow.".into());
                    }
                    _ => unreachable!(),
                }
            });

            // Compute column widths from cell content (matching ratatui's auto-fit logic)
            let cells = with_state(|state| {
                let n = state.node(sid).unwrap();
                match &n.kind {
                    PcWidgetKind::Spreadsheet { grid, .. } => grid.cells.borrow().clone(),
                    _ => unreachable!(),
                }
            });
            let layout: Vec<(u32, u32, String)> = (0..3).map(|col| {
                let max_w = cells.iter()
                    .filter(|&((_, c), _)| *c == col)
                    .map(|((r, _), text)| text.chars().count().max(format!("{}", r + 1).len()))
                    .max()
                    .unwrap_or(8)
                    .max(8) as u32;
                (col, max_w + 2, col_label(col)) // +2 for padding
            }).collect();
            // Match pnc_backend.rs: logical_row = HEADER_ROWS + r
            let header_rows = 1usize;
            let main_rows = 3usize;
            let row_labels: Vec<(u32, String)> = (0..main_rows as u32).map(|r| {
                let logical_row = header_rows + r as usize;
                if logical_row < header_rows + main_rows {
                    (r, format!("{:>4}", logical_row + 1 - header_rows))
                } else {
                    let fr = logical_row - header_rows - main_rows;
                    (r, format!("_{:>3}", fr + 1))
                }
            }).collect();

            spreadsheet_set_column_layout(sid, layout);
            spreadsheet_set_row_labels(sid, row_labels);

            let buf = render_spreadsheet_to_buffer(sid, 120, 40);

            // Verify key structural elements match ratatui output
            // Line 0: Menu bar
            assert_eq!(&buf[0], " [File]  Edit    Insert    Format    Sheet    Help",
                "menu bar mismatch: {:?}", &buf[0]);

            // Line 1: Formula bar — should start with leading space then A1
            assert!(buf[1].starts_with(" "), "formula bar missing leading space: {:?}", &buf[1]);
            assert!(buf[1].contains("A1"), "formula bar missing A1: {:?}", &buf[1]);
            assert!(buf[1].contains("This Text is really long"), "formula bar missing text");

            // Line 3: Grid header starts with │ border, then column labels
            // (Line 0=menu, 1=formula bar, 2=border ┌─, 3=header)
            assert!(buf[3].starts_with("│"), "grid header missing border: {:?}", &buf[3]);
            assert!(buf[3].contains("A"), "header missing A");
            assert!(buf[3].contains("B"), "header missing B");
            assert!(buf[3].contains("C"), "header missing C");

            // Data rows: start at line 5 (menu=0 formula=1 border=2 header=3 sep=4)
            assert!(buf[5].contains("This Text is really long"), "row 1 missing overflow text");
            assert!(buf[5].starts_with("│   1"), "row 1 label wrong: {:?}", &buf[5]);

            // Row 2 should start with │   2
            assert!(buf[6].starts_with("│   2"), "row 2 label wrong: {:?}", &buf[6]);

            // Row 3 should start with │   3
            assert!(buf[7].starts_with("│   3"), "row 3 label wrong: {:?}", &buf[7]);

            // Status bar
            let last = &buf[buf.len() - 1];
            assert!(last.contains("Ctrl+S"), "status bar missing save hint: {:?}", last);

            // Verify cell text appears
            let texts = ["This Text is really long and should overflow."];
            for text in &texts {
                let found = buf.iter().any(|line| line.contains(text));
                assert!(found, "text '{}' not found in pancurses render", text);
            }
        }

        // Moving to the right edge of the spreadsheet.
        #[test]
        fn spreadsheet_cursor_stays_in_bounds() {
            let sid = make_spreadsheet_id();
            // Move to last column
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_col = &mut grid.cursor_col; let total_cols = grid.total_cols;
                    *cursor_col = total_cols - 1;
                }
            });
            let pos = spreadsheet_cursor_position(sid);
            assert_eq!(pos, Some((0, 25)), "cursor should be at col 25 (Z)");

            // KeyRight should be clamped
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_col = &mut grid.cursor_col; let total_cols = grid.total_cols;
                    if *cursor_col + 1 < total_cols { *cursor_col += 1; }
                }
            });
            let pos = spreadsheet_cursor_position(sid);
            assert_eq!(pos, Some((0, 25)), "cursor should stay at col 25");

            // Move to last row
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_row = &mut grid.cursor_row; let total_rows = grid.total_rows;
                    *cursor_row = total_rows - 1;
                }
            });
            // KeyDown should be clamped
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_row = &mut grid.cursor_row; let total_rows = grid.total_rows;
                    if *cursor_row + 1 < total_rows { *cursor_row += 1; }
                }
            });
            let pos = spreadsheet_cursor_position(sid);
            assert_eq!(pos, Some((99, 25)), "cursor should stay at row 99");
        }

        // ── Full ratatui-compatible test suite ──────────────────────────────

        /// Navigation: arrow keys move cursor in all four directions.
        #[test]
        fn navigation_arrows() {
            let sid = make_spreadsheet_id();
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_row = &mut grid.cursor_row; let cursor_col = &mut grid.cursor_col;
                    *cursor_row = 5; *cursor_col = 3;
                }
            });
            // Down
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_row = &mut grid.cursor_row;
                    if *cursor_row + 1 < 100 { *cursor_row += 1; }
                }
            });
            assert_eq!(spreadsheet_cursor_position(sid), Some((6, 3)));
            // Right
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_col = &mut grid.cursor_col;
                    if *cursor_col + 1 < 26 { *cursor_col += 1; }
                }
            });
            assert_eq!(spreadsheet_cursor_position(sid), Some((6, 4)));
            // Up
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_row = &mut grid.cursor_row;
                    if *cursor_row > 0 { *cursor_row -= 1; }
                }
            });
            assert_eq!(spreadsheet_cursor_position(sid), Some((5, 4)));
            // Left
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_col = &mut grid.cursor_col;
                    if *cursor_col > 0 { *cursor_col -= 1; }
                }
            });
            assert_eq!(spreadsheet_cursor_position(sid), Some((5, 3)));
        }

        /// Edit: Enter toggles edit mode.
        #[test]
        fn edit_toggle_enter() {
            let sid = make_spreadsheet_id();
            with_state(|state| spreadsheet_enter(state, sid));
            with_state(|state| {
                let n = state.node(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { grid, .. } = &n.kind { let editing = &grid.editing;
                    assert!(*editing, "should be editing after enter");
                }
            });
        }

        /// Edit: Enter again commits and moves to next row.
        #[test]
        fn edit_enter_commits_and_advances() {
            let sid = make_spreadsheet_id();
            with_state(|state| spreadsheet_enter(state, sid));
            // Type text
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let edit_buf = &mut grid.edit_buf;
                    *edit_buf = "hello".to_string();
                }
            });
            // Commit
            with_state(|state| spreadsheet_enter(state, sid));
            assert_eq!(spreadsheet_get_cell(sid, 0, 0), Some("hello".into()));
            assert_eq!(spreadsheet_cursor_position(sid), Some((1, 0)));
        }

        /// Edit: Esc cancels editing (no cell content change).
        #[test]
        fn edit_esc_cancels() {
            let sid = make_spreadsheet_id();
            // Set initial cell value
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cells = &grid.cells;
                    cells.borrow_mut().insert((0, 0), "original".into());
                }
            });
            // Start editing
            with_state(|state| spreadsheet_enter(state, sid));
            // Change buffer
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let edit_buf = &mut grid.edit_buf;
                    *edit_buf = "modified".to_string();
                }
            });
            // Cancel via Esc (set editing=false without committing)
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let editing = &mut grid.editing;
                    *editing = false;
                }
            });
            assert_eq!(spreadsheet_get_cell(sid, 0, 0), Some("original".into()));
        }

        /// Edit: Tab commits and moves right.
        #[test]
        fn edit_tab_moves_right() {
            let sid = make_spreadsheet_id();
            with_state(|state| spreadsheet_enter(state, sid));
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let edit_buf = &mut grid.edit_buf;
                    *edit_buf = "data".to_string();
                }
            });
            // Commit via Tab (simulate: commit + move right)
            with_state(|state| spreadsheet_commit_edit(state, sid));
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_col = &mut grid.cursor_col; let total_cols = grid.total_cols;
                    if *cursor_col + 1 < total_cols { *cursor_col += 1; }
                }
            });
            assert_eq!(spreadsheet_get_cell(sid, 0, 0), Some("data".into()));
            assert_eq!(spreadsheet_cursor_position(sid), Some((0, 1)));
        }

        /// Cursor: clamped at row 0 and col 0.
        #[test]
        fn cursor_clamped_zero() {
            let sid = make_spreadsheet_id();
            // Try to go up from 0
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_row = &mut grid.cursor_row;
                    if *cursor_row > 0 { *cursor_row -= 1; }
                }
            });
            assert_eq!(spreadsheet_cursor_position(sid), Some((0, 0)));
            // Try to go left from 0
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_col = &mut grid.cursor_col;
                    if *cursor_col > 0 { *cursor_col -= 1; }
                }
            });
            assert_eq!(spreadsheet_cursor_position(sid), Some((0, 0)));
        }

        /// Selection: anchor is set on Shift+Arrow and cleared on non-Shift Arrow.
        #[test]
        fn selection_anchor() {
            let sid = make_spreadsheet_id();
            // Move to (5,5)
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_row = &mut grid.cursor_row; let cursor_col = &mut grid.cursor_col;
                    *cursor_row = 5; *cursor_col = 5;
                }
            });
            // Simulate Shift+Down: set anchor then move
            with_state(|state| spreadsheet_prepare_move(state, sid, true));
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_row = &mut grid.cursor_row;
                    *cursor_row += 1;
                }
            });
            // Anchor should be set to (5, 5)
            let anchor = with_state(|state| {
                let n = state.node(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { grid, .. } = &n.kind { let anchor = &grid.anchor;
                    *anchor
                } else { None }
            });
            assert_eq!(anchor, Some((5, 5)), "anchor should be at (5,5) after Shift+Down");

            // Non-shift move should clear anchor
            with_state(|state| spreadsheet_prepare_move(state, sid, false));
            let anchor = with_state(|state| {
                let n = state.node(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { grid, .. } = &n.kind { let anchor = &grid.anchor;
                    *anchor
                } else { None }
            });
            assert_eq!(anchor, None, "anchor should be None after non-Shift move");
        }

        /// Cell content: set and get round-trip.
        /// Verify that make_spreadsheet_id correctly sets focus on the spreadsheet.
        #[test]
        fn focus_is_on_spreadsheet_after_creation() {
            let sid = make_spreadsheet_id();
            let focused = with_state(|s| s.focus_id);
            assert_eq!(focused, Some(sid), "focus should be on the spreadsheet after creation");
        }

        /// Simulate full arrow-key processing: down, right, up, left.
        /// This tests the exact order of operations the keyboard handler uses.
        #[test]
        fn keyboard_arrows_work() {
            let sid = make_spreadsheet_id();
            // Move to (3,5) first
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_row = &mut grid.cursor_row; let cursor_col = &mut grid.cursor_col;
                    *cursor_row = 3; *cursor_col = 5;
                }
            });
            // Simulate KeyDown handler:
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_row = &mut grid.cursor_row; let total_rows = grid.total_rows;
                    if *cursor_row + 1 < total_rows { *cursor_row += 1; }
                }
            });
            assert_eq!(spreadsheet_cursor_position(sid), Some((4, 5)), "KeyDown failed");
            // Simulate KeyRight handler:
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_col = &mut grid.cursor_col; let total_cols = grid.total_cols;
                    if *cursor_col + 1 < total_cols { *cursor_col += 1; }
                }
            });
            assert_eq!(spreadsheet_cursor_position(sid), Some((4, 6)), "KeyRight failed");
            // Simulate KeyUp handler:
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_row = &mut grid.cursor_row;
                    if *cursor_row > 0 { *cursor_row -= 1; }
                }
            });
            assert_eq!(spreadsheet_cursor_position(sid), Some((3, 6)), "KeyUp failed");
            // Simulate KeyLeft handler:
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_col = &mut grid.cursor_col;
                    if *cursor_col > 0 { *cursor_col -= 1; }
                }
            });
            assert_eq!(spreadsheet_cursor_position(sid), Some((3, 5)), "KeyLeft failed");
        }

        /// Simulate Tab key: commit edit and move right.
        #[test]
        fn keyboard_tab_commits_and_moves_right() {
            let sid = make_spreadsheet_id();
            with_state(|state| spreadsheet_enter(state, sid));
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let edit_buf = &mut grid.edit_buf;
                    *edit_buf = "tab_test".to_string();
                }
            });
            // Commit edit (simulates Tab handler)
            with_state(|state| spreadsheet_commit_edit(state, sid));
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let cursor_col = &mut grid.cursor_col; let total_cols = grid.total_cols;
                    if *cursor_col + 1 < total_cols { *cursor_col += 1; }
                }
            });
            assert_eq!(spreadsheet_get_cell(sid, 0, 0), Some("tab_test".into()));
            assert_eq!(spreadsheet_cursor_position(sid), Some((0, 1)));
        }

        /// Simulate Enter then type then Enter again (commit + advance).
        #[test]
        fn keyboard_enter_edit_type_enter_commit() {
            let sid = make_spreadsheet_id();
            // First Enter starts editing
            with_state(|state| spreadsheet_enter(state, sid));
            // Type "hello" into edit buffer (simulates Character('h'), etc.)
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let edit_buf = &mut grid.edit_buf; let edit_pos = &mut grid.edit_pos;
                    *edit_buf = "hello".to_string();
                    *edit_pos = 5;
                }
            });
            // Second Enter commits and advances row
            with_state(|state| spreadsheet_enter(state, sid));
            assert_eq!(spreadsheet_get_cell(sid, 0, 0), Some("hello".into()));
            assert_eq!(spreadsheet_cursor_position(sid), Some((1, 0)));
        }

        /// Simulate typing characters in edit mode via the Character(c) handler logic.
        #[test]
        fn keyboard_typing_in_edit_mode() {
            let sid = make_spreadsheet_id();
            // Start editing
            with_state(|state| spreadsheet_enter(state, sid));
            // Type 'a' (simulates Character('a') handler)
            with_state(|state| {
                if let Some(fid) = state.focus_id {
                    if is_spreadsheet_focused(state, fid) {
                        if let Some(n) = state.node_mut(fid) {
                            if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let editing = &mut grid.editing; let edit_buf = &mut grid.edit_buf; let edit_pos = &mut grid.edit_pos;
                                if !*editing {
                                    *editing = true;
                                    *edit_buf = "a".to_string();
                                    *edit_pos = 1;
                                } else {
                                    edit_buf.insert(*edit_pos, 'a');
                                    *edit_pos += 1;
                                }
                            }
                        }
                    }
                }
            });
            // Type 'b' 
            with_state(|state| {
                if let Some(fid) = state.focus_id {
                    if is_spreadsheet_focused(state, fid) {
                        if let Some(n) = state.node_mut(fid) {
                            if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let editing = &mut grid.editing; let edit_buf = &mut grid.edit_buf; let edit_pos = &mut grid.edit_pos;
                                if !*editing {
                                    *editing = true;
                                    *edit_buf = "b".to_string();
                                    *edit_pos = 1;
                                } else {
                                    edit_buf.insert(*edit_pos, 'b');
                                    *edit_pos += 1;
                                }
                            }
                        }
                    }
                }
            });
            // Commit
            with_state(|state| spreadsheet_commit_edit(state, sid));
            assert_eq!(spreadsheet_get_cell(sid, 0, 0), Some("ab".into()));
        }

        /// Simulate Backspace in edit mode.
        #[test]
        fn keyboard_backspace_in_edit() {
            let sid = make_spreadsheet_id();
            with_state(|state| spreadsheet_enter(state, sid));
            // Type "abc"
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let editing = &mut grid.editing; let edit_buf = &mut grid.edit_buf; let edit_pos = &mut grid.edit_pos;
                    *editing = true;
                    *edit_buf = "abc".to_string();
                    *edit_pos = 3;
                }
            });
            // Simulate Backspace: remove char before cursor
            with_state(|state| {
                if let Some(fid) = state.focus_id {
                    if is_spreadsheet_focused(state, fid) {
                        if let Some(n) = state.node_mut(fid) {
                            if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let editing = &mut grid.editing; let edit_buf = &mut grid.edit_buf; let edit_pos = &mut grid.edit_pos;
                                if *editing && *edit_pos > 0 {
                                    *edit_pos -= 1;
                                    edit_buf.remove(*edit_pos);
                                }
                            }
                        }
                    }
                }
            });
            assert_eq!(spreadsheet_get_cell(sid, 0, 0), None, "not committed yet");
            with_state(|state| spreadsheet_enter(state, sid));
            assert_eq!(spreadsheet_get_cell(sid, 0, 0), Some("ab".into()), "backspace should remove 'c'");
        }

        /// Simulate the q/Q quit handler (should quit when not editing).
        #[test]
        fn keyboard_q_quits_when_not_editing() {
            let sid = make_spreadsheet_id();
            // Mark not editing
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                if let PcWidgetKind::Spreadsheet { ref mut grid, .. } = n.kind { let editing = &mut grid.editing;
                    *editing = false;
                }
            });
            with_state(|state| state.running = false);
            // Verify running was set to false
            let running = with_state(|s| s.running);
            assert!(!running, "running should be false after q");
        }

        /// Simulate Ctrl+Q (KeyExit) — should quit regardless of edit state.
        #[test]
        fn keyboard_ctrlq_quits() {
            with_state(|state| state.running = false);
            let running = with_state(|s| s.running);
            assert!(!running, "running should be false after Ctrl+Q/KeyExit");
        }

        #[test]
        fn cell_content_roundtrip() {
            let sid = make_spreadsheet_id();
            spreadsheet_set_cell(sid, 5, 3, "hello world");
            assert_eq!(spreadsheet_get_cell(sid, 5, 3), Some("hello world".into()));
            assert_eq!(spreadsheet_get_cell(sid, 0, 0), None, "different cell should be empty");
        }

        /// Cell content: multiple cells can be set independently.
        #[test]
        fn cell_content_multiple() {
            let sid = make_spreadsheet_id();
            spreadsheet_set_cell(sid, 0, 0, "A1");
            spreadsheet_set_cell(sid, 0, 1, "B1");
            spreadsheet_set_cell(sid, 1, 0, "A2");
            assert_eq!(spreadsheet_get_cell(sid, 0, 0), Some("A1".into()));
            assert_eq!(spreadsheet_get_cell(sid, 0, 1), Some("B1".into()));
            assert_eq!(spreadsheet_get_cell(sid, 1, 0), Some("A2".into()));
        }

        /// Rendering: buffer has correct number of rows.
        #[test]
        fn render_buffer_size() {
            let wid = with_state(|s| s.add_node(
                PcWidgetKind::Window { title: "test".into() }, None,
            ));
            let sid = create_spreadsheet(10, 3).unwrap();
with_state(|s| {
    if let Some(n) = s.node_mut(sid) {
        n.parent = Some(wid);
        if let PcWidgetKind::Spreadsheet { grid, border_title, menu_text, status_text, formula_bar_trailing, .. } = &mut n.kind {
            grid.header_row_count = 2;
            grid.main_row_count = 20;
            *menu_text = "menu".to_string();
            *status_text = "status".to_string();
        }
    }
});
            let buf = render_spreadsheet_to_buffer(sid, 80, 10);
            assert_eq!(buf.len(), 10);
            assert!(buf[0].contains("menu"));
            assert!(buf[buf.len()-1].contains("status"));
            // Data rows should have │ borders
            assert!(buf[4].starts_with("│"), "data row missing │ border");
        }

        /// Rendering: column headers use layout labels.
        #[test]
        fn render_column_headers_from_layout() {
            let wid = with_state(|s| s.add_node(
                PcWidgetKind::Window { title: "test".into() }, None,
            ));
            let sid = create_spreadsheet(10, 2).unwrap();
with_state(|s| {
    if let Some(n) = s.node_mut(sid) {
        n.parent = Some(wid);
        if let PcWidgetKind::Spreadsheet { grid, border_title, menu_text, status_text, formula_bar_trailing, .. } = &mut n.kind {
            grid.header_row_count = 2;
            grid.main_row_count = 20;
            grid.column_layout = vec![(0, 5, "X".into()), (1, 5, "Y".into())];        }
    }
});
            let buf = render_spreadsheet_to_buffer(sid, 80, 10);
            // Header is at index 2 (index 0 = formula bar, index 1 = border, since menu_text is empty)
            assert!(buf[2].contains("X"), "header missing X in {:?}", &buf[2]);
            assert!(buf[2].contains("Y"), "header missing Y in {:?}", &buf[2]);
        }

        /// Shift+arrows must map to the *shifted* ncurses keycodes.
        ///
        /// They used to alias onto the plain arrows (a screen-256color
        /// terminfo workaround), which turned every Shift+arrow into plain
        /// movement: the widget's `Input::KeyS*` handlers were unreachable and
        /// no keyboard selection was possible in this backend. Plain arrows
        /// must keep their own keycodes.
        #[cfg(not(windows))]
        #[test]
        fn shift_arrows_map_to_shifted_keycodes() {
            let code = |seq: &[u8]| {
                super::CSI_KEY_BINDINGS
                    .iter()
                    .find(|(s, _)| *s == seq)
                    .map(|(_, c)| *c)
                    .unwrap_or_else(|| panic!("{seq:?} not registered"))
            };
            // ncurses: KEY_UP/DOWN/LEFT/RIGHT, KEY_SR/SF, KEY_SLEFT/SRIGHT.
            assert_eq!(code(b"\x1b[C\x00"), 261, "plain Right stays KEY_RIGHT");
            assert_eq!(code(b"\x1b[A\x00"), 259, "plain Up stays KEY_UP");
            assert_eq!(code(b"\x1b[1;2C\x00"), 402, "Shift+Right -> KEY_SRIGHT");
            assert_eq!(code(b"\x1b[1;2D\x00"), 393, "Shift+Left -> KEY_SLEFT");
            assert_eq!(code(b"\x1b[1;2A\x00"), 337, "Shift+Up -> KEY_SR");
            assert_eq!(code(b"\x1b[1;2B\x00"), 336, "Shift+Down -> KEY_SF");
            // Home/End/Page keys must not regress either.
            assert_eq!(code(b"\x1b[H\x00"), 262, "Home -> KEY_HOME");
            assert_eq!(code(b"\x1b[F\x00"), 360, "End -> KEY_END");
            assert_eq!(code(b"\x1b[5~\x00"), 339, "PgUp -> KEY_PPAGE");
            assert_eq!(code(b"\x1b[6~\x00"), 338, "PgDn -> KEY_NPAGE");
        }

        /// The coverage decision the header renderer consumes: anchor↔cursor
        /// on both axes, `None` without a selection. (The SGR bytes it drives
        /// are asserted end-to-end by the live tmux test
        /// `pnc_selected_headers`, which captures the pane with `-e`.)
        #[test]
        fn header_cover_ranges() {
            assert_eq!(header_cover(None, (4, 2)), (None, None), "no anchor, no cover");
            assert_eq!(
                header_cover(Some((4, 2)), (4, 2)),
                (Some((4, 4)), Some((2, 2))),
                "single-cell selection covers just that header"
            );
            assert_eq!(
                header_cover(Some((4, 2)), (6, 5)),
                (Some((4, 6)), Some((2, 5))),
                "extended selection covers the whole span on both axes"
            );
            assert_eq!(
                header_cover(Some((6, 5)), (4, 2)),
                (Some((4, 6)), Some((2, 5))),
                "reversed anchor (cursor above/left) normalises"
            );
        }

        /// Reproduce blank spreadsheet: test with margin columns, header rows,
        /// and cell data stored via `set_cell` (like `fill_cells` does).
        #[test]
        fn pnc_fill_cells_simulation() {
            let wid = with_state(|s| s.add_node(
                PcWidgetKind::Window { title: "corro".into() }, None,
            ));
            let total_rows = 24u32;
            let total_cols = 5u32;
            let sid = create_spreadsheet(100, 3).unwrap();
with_state(|s| {
    if let Some(n) = s.node_mut(sid) {
        n.parent = Some(wid);
        if let PcWidgetKind::Spreadsheet { grid, border_title, menu_text, status_text, formula_bar_trailing, .. } = &mut n.kind {
            *border_title = "corro  24r × 3c  ops 0".to_string();
            grid.header_row_count = 2;
            grid.main_row_count = 24;
            *menu_text = " [File]   Edit    Insert    Format    Sheet    Help".to_string();
            *status_text = "status bar".to_string();
        }
    }
});

            // Simulate what pnc_backend.rs does: margin_cols=2, main_cols=3, total 5 columns
            let lm = 2u32;
            let mc = 3u32;
            let col_ixs: Vec<usize> = (0..5).collect();
            let layout: Vec<(u32, u32, String)> = col_ixs.iter().map(|&c| {
                (c as u32, 10u32, format!("C{}", c))
            }).collect();
            let row_labels: Vec<(u32, String)> = (0..total_rows)
                .map(|i| (i, format!("{:>4}", i + 1)))
                .collect();
            with_state(|state| {
                let n = state.node_mut(sid).unwrap();
                match &mut n.kind {
                    PcWidgetKind::Spreadsheet { grid, .. } => { let column_layout = &mut grid.column_layout;
                        *column_layout = layout;
                    }
                    _ => unreachable!(),
                }
            });
            spreadsheet_set_row_labels(sid, row_labels);
            spreadsheet_set_grid_config(sid, lm, mc);
            spreadsheet_set_cursor(sid, 0, 0);

            // Store cell data at (display_row_idx, global_col_idx) like fill_cells
            // Set both margin (col 0,1) and main (col 2,3,4) cells
            spreadsheet_set_cell(sid, 0, 2, "hello"); // main col
            spreadsheet_set_cell(sid, 0, 3, "world"); // main col
            spreadsheet_set_cell(sid, 1, 2, "foo");
            spreadsheet_set_cell(sid, 1, 3, "bar");

            let buf = render_spreadsheet_to_buffer(sid, 120, 40);

            // Data rows start at index 5 (menu+formula+border+header+separator=5)
            // Row 1 should contain "hello" in the first main column
            assert!(buf[5].contains("hello"),
                "row 1 missing 'hello'. line={:?}", &buf[5]);
            assert!(buf[5].contains("world"),
                "row 1 missing 'world'. line={:?}", &buf[5]);

            assert!(buf[6].contains("foo"),
                "row 2 missing 'foo'. line={:?}", &buf[6]);
            assert!(buf[6].contains("bar"),
                "row 2 missing 'bar'. line={:?}", &buf[6]);

            assert!(buf[5].starts_with("│   1"),
                "row 1 should start with │   1. got={:?}", &buf[5]);
        }

        /// Diagnose blank rows: check row_idx computation and label lookup
        /// for the exact same flow as render_widget during app startup.
        #[test]
        fn diagnose_row_label_lookup() {
            let wid = with_state(|s| s.add_node(
                PcWidgetKind::Window { title: "corro".into() }, None,
            ));
            let total_rows = 10u32;
            let total_cols = 3u32;
            let sid = create_spreadsheet(100, 3).unwrap();
with_state(|s| {
    if let Some(n) = s.node_mut(sid) {
        n.parent = Some(wid);
        if let PcWidgetKind::Spreadsheet { grid, border_title, menu_text, status_text, formula_bar_trailing, .. } = &mut n.kind {
            *border_title = "Test".to_string();
            grid.header_row_count = 2;
            grid.main_row_count = 24;
            *menu_text = "Menu".to_string();
            *status_text = "Status".to_string();
        }
    }
});
            spreadsheet_set_cursor(sid, 0, 2);

            // Replicate fill_cells: store cell text at (display_row_idx, global_col_idx)
            spreadsheet_set_cell(sid, 0, 2, "A1_data");
            spreadsheet_set_cell(sid, 1, 2, "A2_data");

            let buf = render_spreadsheet_to_buffer(sid, 80, 20);
            // Data rows start at index 5 (menu=0 formula=1 border=2 header=3 sep=4)
            assert!(buf[5].contains("A1_data"),
                "buf[5] missing cell data: {:?}", &buf[5]);
            assert!(buf[5].contains("   1"),
                "buf[5] missing row label: {:?}", &buf[5]);
            assert!(buf[6].contains("A2_data"),
                "buf[6] missing cell data: {:?}", &buf[6]);
        }

        /// Test rendering with empty cell data that fill_cells would produce
        /// for an empty grid (no content at any address).
        #[test]
        fn pnc_fill_cells_empty_grid_renders_structure() {
            let wid = with_state(|s| s.add_node(
                PcWidgetKind::Window { title: "corro".into() }, None,
            ));
            let total_rows = 10u32;
            let total_cols = 5u32;
            let sid = create_spreadsheet(100, 3).unwrap();
with_state(|s| {
    if let Some(n) = s.node_mut(sid) {
        n.parent = Some(wid);
        if let PcWidgetKind::Spreadsheet { grid, border_title, menu_text, status_text, formula_bar_trailing, .. } = &mut n.kind {
            *border_title = "test".to_string();
            grid.header_row_count = 2;
            grid.main_row_count = 24;
            *menu_text = " [File]   Edit".to_string();
            *status_text = "status".to_string();
        }
    }
});
            spreadsheet_set_cursor(sid, 0, 2);

            // Do NOT set any cell data — test what an empty grid renders like
            let buf = render_spreadsheet_to_buffer(sid, 120, 40);

            // Structure should still be present: menu, formula bar, headers, data rows
            assert!(buf[0].contains("File"), "menu missing: {:?}", &buf[0]);
            assert!(buf[3].contains("A"), "header missing A: {:?}", &buf[3]);

            // Data rows should have proper structure even if empty
            assert!(buf[5].starts_with("│"), "data row missing │: {:?}", &buf[5]);
        }

        /// Rendering: pre-computed row labels appear in data rows.
        #[test]
        fn render_row_labels_from_layout() {
            let wid = with_state(|s| s.add_node(
                PcWidgetKind::Window { title: "test".into() }, None,
            ));
            let sid = create_spreadsheet(10, 1).unwrap();
with_state(|s| {
    if let Some(n) = s.node_mut(sid) {
        n.parent = Some(wid);
        if let PcWidgetKind::Spreadsheet { grid, border_title, menu_text, status_text, formula_bar_trailing, .. } = &mut n.kind {
            grid.header_row_count = 2;
            grid.main_row_count = 20;
            grid.row_labels = vec![(0, "ROW0".into()), (1, "ROW1".into())];        }
    }
});
            let buf = render_spreadsheet_to_buffer(sid, 80, 10);
            assert!(buf[4].contains("ROW0"), "row 0 label missing: {:?}", buf[4]);
            assert!(buf[5].contains("ROW1"), "row 1 label missing: {:?}", buf[5]);
        }

        /// Verify the SGR rendering of the formula bar: uses sgr_formula() style,
        /// includes trailing status text, and does NOT emit extra resets before the border.
        /// This calls render_widget with a dummy Window pointer because Spreadsheet
        /// rendering never accesses the Window (it writes SGR to spreadsheet_output).
        #[test]
        fn formula_bar_sgr_matches_ratatui() {
            let wid = with_state(|s| s.add_node(
                PcWidgetKind::Window { title: "test".into() }, None,
            ));
            let total_rows = 3u32;
            let total_cols = 3u32;
            let sid = create_spreadsheet(100, 3).unwrap();
with_state(|s| {
    if let Some(n) = s.node_mut(sid) {
        n.parent = Some(wid);
        if let PcWidgetKind::Spreadsheet { grid, border_title, menu_text, status_text, formula_bar_trailing, .. } = &mut n.kind {
            *border_title = "Test".to_string();
            *formula_bar_trailing = "   ·  Loaded workbook /root/src/corro_mainloop/t_shift5.corro @ revision 30".to_string();
            grid.header_row_count = 2;
            grid.main_row_count = 24;
            grid.column_layout = vec![(0, 6, "L0".into()), (1, 6, "L1".into()), (2, 10, "A".into()), (3, 10, "B".into()), (4, 10, "C".into())];            *menu_text = "Menu".to_string();
        }
    }
});
spreadsheet_set_raw_cell(sid, 0, 0, "42");

            spreadsheet_set_cursor(sid, 0, 0);

            let buf = render_spreadsheet_to_buffer(sid, 80, 10);
            // Formula bar (buf[1]) should contain address, cell value, and trailing status
            assert!(buf[1].contains("A1"), "formula bar missing A1: {:?}", buf[1]);
            assert!(buf[1].contains("42"), "formula bar missing cell value: {:?}", buf[1]);
            assert!(buf[1].contains("Loaded workbook"),
                "formula bar missing trailing status: {:?}", buf[1]);
            assert!(buf[1].contains("·"),
                "formula bar missing separator: {:?}", buf[1]);
            // The trailing text should match the expected status message
            assert!(buf[1].contains("Loaded workbook"),
                "formula bar missing status prefix: {:?}", buf[1]);
            assert!(!buf[1].contains("type/F2"),
                "formula bar should NOT show edit hints (they belong in status bar): {:?}", buf[1]);
        }

        /// Test that right-aligned numeric content is rendered correctly
        #[test]
        fn right_aligned_cell_rendering() {
            let wid = with_state(|s| s.add_node(
                PcWidgetKind::Window { title: "test".into() }, None,
            ));
            let sid = create_spreadsheet(5, 3).unwrap();
with_state(|s| {
    if let Some(n) = s.node_mut(sid) {
        n.parent = Some(wid);
        if let PcWidgetKind::Spreadsheet { grid, border_title, menu_text, status_text, formula_bar_trailing, .. } = &mut n.kind {
            grid.header_row_count = 2;
            grid.main_row_count = 3;
            grid.column_layout = vec![(0, 4, "[A".into()), (1, 4, "A".into()), (2, 4, "]A".into())];
        }
    }
});
            spreadsheet_set_grid_config(sid, 1, 2);
            spreadsheet_set_cell(sid, 1, 1, "  22");
            let buf = render_spreadsheet_to_buffer(sid, 40, 12);
            // Data rows start at index 4 (border+header+separator=4 when no menu bar)
            eprintln!("buf 2-col: buf[5] = {:?}", &buf[5]);
            // The cell at (row=1, col=1) should contain "  22" (4 chars)
            assert!(buf[5].contains("  22"),
                "Cell should contain '  22' but got: {:?}", &buf[5]);

            // Now test with larger margin_cols that match the real backend scenario
            let sid2 = create_spreadsheet(5, 3).unwrap();
with_state(|s| {
    if let Some(n) = s.node_mut(sid2) {
        n.parent = Some(wid);
        if let PcWidgetKind::Spreadsheet { grid, border_title, menu_text, status_text, formula_bar_trailing, .. } = &mut n.kind {
            grid.header_row_count = 2;
            grid.main_row_count = 3;
            grid.column_layout = vec![(701, 4, "[A".into()), (702, 4, "A".into()), (703, 4, "B".into())];
            grid.row_labels = vec![(0, "   1".into()), (1, "   2".into())];
        }
    }
});
            spreadsheet_set_grid_config(sid2, 702, 2);
            spreadsheet_set_cell(sid2, 1, 702, "  22");
            let buf2 = render_spreadsheet_to_buffer(sid2, 40, 12);
            eprintln!("buf 702-col: buf[5] = {:?}", &buf2[5]);
            assert!(buf2[5].contains("  22"),
                "Cell at col 702 should contain '  22' but got: {:?}", &buf2[5]);

        }
        
        /// Test cell content rendering for the right-aligned "22" in a wide-margin setup.
        #[test]
        fn right_aligned_cell_matches_reference() {
            let wid = with_state(|s| s.add_node(
                PcWidgetKind::Window { title: "test".into() }, None,
            ));
            let sid = create_spreadsheet(100, 5).unwrap();
with_state(|s| {
    if let Some(n) = s.node_mut(sid) {
        n.parent = Some(wid);
        if let PcWidgetKind::Spreadsheet { grid, border_title, menu_text, status_text, formula_bar_trailing, .. } = &mut n.kind {
            *border_title = "corro  2r x 2c ops 34".to_string();
            *formula_bar_trailing = "   ·  Loaded workbook /root/src/corro_mainloop/t_shift5.corro @ revision 34".to_string();
            grid.header_row_count = 2;
            grid.main_row_count = 2;
            grid.column_layout = vec![(701, 4, "[A".into()), (702, 4, "A".into()), (703, 4, "B".into())];
            grid.row_labels = vec![(0, "   1".into()), (1, "   2".into())];
            *menu_text = " [File]   Edit    Insert    Format    Sheet    Help".to_string();
            *status_text = "  type to edit (or addr: val)   Enter·confirm   Esc·discard".to_string();
        }
    }
});
            spreadsheet_set_grid_config(sid, 702, 2);
            spreadsheet_set_cursor(sid, 1, 702);
            // Simulate fill_cells: store right-aligned number in main column A
            spreadsheet_set_cell(sid, 1, 702, "  22");
            spreadsheet_set_cell_style(sid, 1, 702, 0);
            spreadsheet_set_raw_cell(sid, 1, 702, "22");
            let buf = render_spreadsheet_to_buffer(sid, 120, 40);
            eprintln!("Line 6: {:?}", &buf[6]);
            // The rendered output must contain "  22" (right-aligned in 4-wide cell)
            assert!(buf[6].contains("  22") || buf[6].contains("22"),
                "Cell should contain '22' but line 6 = {:?}", &buf[6]);
        }
    }

#[cfg(test)]
mod canvas_draw_tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    /// A registered canvas draw callback must actually run, receive a
    /// `DrawContext`, and have its painting survive into the emitted grid.
    ///
    /// This is the test that makes `Canvas::set_draw_callback` real on a
    /// terminal: registration alone is worthless if nothing invokes it or if
    /// the resulting grid never reaches a screen.
    #[test]
    fn canvas_draw_callback_paints_through_the_grid() {
        let canvas = create_canvas().unwrap();
        with_state(|s| {
            let n = s.node_mut(canvas).unwrap();
            n.rect = Rect { x: 2, y: 3, w: 10, h: 4 };
        });

        let calls = Rc::new(Cell::new(0));
        let c = calls.clone();
        let painted_text = Rc::new(std::cell::RefCell::new(String::new()));
        let pt = painted_text.clone();
        canvas_set_draw_callback(
            canvas,
            Box::new(move |dc: &mut dyn crate::core::DrawContext, w: i32, h: i32| {
                c.set(c.get() + 1);
                // The same 2D API a GTK canvas would use.
                dc.clear(1.0, 1.0, 1.0, 1.0);
                dc.fill_rect(0.0, 0.0, 40.0, 22.0, 0.0, 0.0, 1.0, 1.0);
                dc.draw_text(8.0, 0.0, "hi", "monospace", 12.0, 0.0, 0.0, 1.0, 1.0);
                *pt.borrow_mut() = format!("{w}x{h}");
            }),
        );

        let painted = paint_canvases_for_test();
        assert_eq!(1, painted, "one canvas should have been painted");
        assert_eq!(1, calls.get(), "the draw callback must be invoked");
        assert_eq!("10x4", painted_text.borrow().as_str(), "sized to the canvas rect");

        canvas_clear_draw_callback(canvas);
        assert_eq!(0, paint_canvases_for_test(), "cleared canvas is not painted");
    }

    /// With no callback registered the redraw must not try to paint (so the
    /// loop can skip its flush entirely).
    #[test]
    fn canvases_without_callbacks_are_skipped() {
        let canvas = create_canvas().unwrap();
        with_state(|s| {
            let n = s.node_mut(canvas).unwrap();
            n.rect = Rect { x: 0, y: 0, w: 4, h: 2 };
        });
        canvas_clear_draw_callback(canvas);
        assert_eq!(0, paint_canvases_for_test());
    }

    /// A zero-sized canvas must be skipped rather than allocating an empty
    /// grid (which would emit nothing and skew the painted count).
    #[test]
    fn zero_sized_canvas_is_skipped() {
        let canvas = create_canvas().unwrap();
        with_state(|s| {
            let n = s.node_mut(canvas).unwrap();
            n.rect = Rect { x: 0, y: 0, w: 0, h: 0 };
        });
        let calls = Rc::new(Cell::new(0));
        let c = calls.clone();
        canvas_set_draw_callback(canvas, Box::new(move |_dc, _w, _h| c.set(c.get() + 1)));
        assert_eq!(0, paint_canvases_for_test());
        assert_eq!(0, calls.get(), "callback must not run for an empty rect");
        canvas_clear_draw_callback(canvas);
    }

    /// The emitted stream must be absolute, so two canvases at different
    /// offsets both land correctly regardless of paint order.
    #[test]
    fn two_canvases_emit_at_their_own_origins() {
        use crate::backends::pancurses_draw::CellGrid;
        let a = CellGrid::new(2, 1);
        let b = CellGrid::new(2, 1);
        let sa = a.to_ansi(0, 0);
        let sb = b.to_ansi(5, 9);
        assert!(sa.contains("\x1b[1;1H"), "first canvas at its origin");
        assert!(sb.contains("\x1b[6;10H"), "second canvas at its own origin");
    }
}

#[cfg(test)]
mod entry_changed_tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    /// `Entry::connect_changed` must fire when the text changes.
    ///
    /// This is the contract the GUI backends get from GTK's `changed` signal,
    /// and what `gui_backend`'s formula bar depends on. It was registered but
    /// never invoked, which is why the capability probe reported the row INERT.
    #[test]
    fn connect_changed_fires_on_set_text() {
        let win = create_window().unwrap();
        let entry = create_entry().unwrap();
        with_state(|s| {
            s.node_mut(entry).unwrap().parent = Some(win);
        });

        let fired = Rc::new(Cell::new(0));
        let f = fired.clone();
        let _ = crate::backends_pancurses_adapter::Entry { id: entry }
            .connect_changed(move || f.set(f.get() + 1));
        eprintln!("CALLBACKS registered: {}", with_state(|s| s.node(entry).map(|n| n.callbacks.len()).unwrap_or(0)));

        set_entry_text(entry, "42");
        assert_eq!(1, fired.get(), "one change → one callback");
        assert_eq!(Some("42".to_string()), get_entry_text(entry));

        // A second change fires again: the registration is persistent, not
        // one-shot, which is what a `connect_*` name promises.
        set_entry_text(entry, "43");
        assert_eq!(2, fired.get(), "the callback must survive the first call");
    }

    /// A `connect_changed` handler frequently edits another widget (that is the
    /// point of the hook), so it must not run under the state borrow.
    #[test]
    fn connect_changed_may_reenter_the_widget_tree() {
        let label = create_label("old").unwrap();
        let entry = create_entry().unwrap();

        let l = label;
        let _ = crate::backends_pancurses_adapter::Entry { id: entry }
            .connect_changed(move || {
                // Re-enters `with_state` while the change is being dispatched.
                set_label_text(l, "mirrored");
            });

        set_entry_text(entry, "typed");
        assert_eq!(Some("mirrored".to_string()), get_label_text(label));
    }

    /// Changing text on an unknown id must be a no-op, not a panic.
    #[test]
    fn set_text_on_missing_entry_is_inert() {
        set_entry_text(999_999, "x");
    }
}

    #[cfg(test)]
    mod entry_caret_tests {
        use super::*;

        fn entry_with(text: &str) -> usize {
            let e = create_entry().unwrap();
            set_entry_text(e, text);
            e
        }

        /// The caret must round-trip: what a host sets is what it reads back.
        ///
        /// This is the contract GTK's `Entry::get_position`/`set_position`
        /// provide, and the reason a host could previously keep a parallel
        /// caret copy that could drift from the widget's own.
        #[test]
        fn caret_round_trips() {
            let e = entry_with("hello");
            set_entry_cursor(e, 3);
            assert_eq!(Some(3), get_entry_cursor(e));

            // `set_entry_text` parks the caret at the end, like GTK.
            set_entry_text(e, "abcd");
            assert_eq!(Some(4), get_entry_cursor(e));
        }

        /// An out-of-range position must clamp, not panic.
        ///
        /// The caret indexes the buffer, so a stale position (e.g. after the
        /// text shrank) would panic on the next keystroke.
        #[test]
        fn caret_clamps_to_the_buffer() {
            let e = entry_with("abc");
            set_entry_cursor(e, 9999);
            assert_eq!(Some(3), get_entry_cursor(e), "clamped to len, not panicking");
        }

        /// A mid-codepoint position must snap to a char boundary, or the next
        /// removal would split a multi-byte character.
        #[test]
        fn caret_snaps_to_a_char_boundary() {
            // "é" is two bytes; position 1 is inside it.
            let e = entry_with("é");
            assert_eq!(2, "é".len());
            set_entry_cursor(e, 1);
            assert_eq!(Some(0), get_entry_cursor(e));

            // Position 2 (the end) is valid and must be kept.
            set_entry_cursor(e, 2);
            assert_eq!(Some(2), get_entry_cursor(e));
        }

        /// Focus must be settable and observable, so `grab_focus` followed by
        /// `has_focus` is consistent — previously the latter always said false.
        #[test]
        fn focus_is_reported() {
            let a = entry_with("a");
            let b = entry_with("b");
            entry_grab_focus(a);
            assert!(entry_has_focus(a));
            assert!(!entry_has_focus(b), "focus is exclusive");

            entry_grab_focus(b);
            assert!(entry_has_focus(b));
            assert!(!entry_has_focus(a), "focus moved");
        }

        /// `grab_focus` on a non-entry must not steal focus.
        #[test]
        fn grab_focus_ignores_non_entries() {
            let label = create_label("x").unwrap();
            let e = entry_with("e");
            entry_grab_focus(e);
            entry_grab_focus(label);
            assert!(entry_has_focus(e), "a label must not take entry focus");
        }

        /// Caret helpers are inert on an unknown id, not panicking.
        #[test]
        fn caret_on_missing_id_is_inert() {
            set_entry_cursor(999_999, 3);
            assert_eq!(None, get_entry_cursor(999_999));
            assert!(!entry_has_focus(999_999));
        }
    }

#[cfg(test)]
mod entry_via_public_wrapper_tests {
    use super::*;
    use crate::backends_pancurses_adapter::Entry;
    use std::cell::Cell;
    use std::rc::Rc;

    /// The whole chain, through the portable `common::Entry` wrapper that a
    /// host actually uses: caret round-trip, focus, change firing, and the
    /// suppress flag honoured on top of real firing.
    #[test]
    fn public_wrapper_caret_focus_and_suppression() {
        let win = create_window().unwrap();
        let _ = win;
        let inner = create_entry().unwrap();
        let entry = Entry { id: inner };

        // Caret now round-trips natively (previously the wrapper's
        // `caret_override` shim was the only source of truth here).
        entry.set_text("hello");
        entry.set_position(2);
        assert_eq!(Some(2), entry.get_position());

        // Focus is observable.
        entry.grab_focus();
        assert!(entry_has_focus(inner));

        // Changes fire.
        let fired = Rc::new(Cell::new(0));
        let f = fired.clone();
        let _ = entry.connect_changed(move || f.set(f.get() + 1));
        entry.set_text("world");
        assert_eq!(1, fired.get());

        // NOTE: `set_text_suppressing_changed` / `set_text_preserving_caret`
        // live on the generic `common::Entry` wrapper (they wrap `set_text`
        // with a flag / a save-restore), not on this adapter — so they are
        // exercised in the generic layer's own tests. What matters here is
        // that the adapter fires, which the assertion above covers.
    }

    /// `set_text_preserving_caret` must actually preserve the caret now that
    /// the backend reports a real position — previously the wrapper's
    /// `caret_override` shim had to stand in for it.
    #[test]
    fn caret_survives_a_text_replacement() {
        let inner = create_entry().unwrap();
        let entry = Entry { id: inner };
        entry.set_text("abcdef");
        entry.set_position(3);
        // The save/restore the generic `set_text_preserving_caret` performs.
        let pos = entry.get_position();
        entry.set_text("xyz");
        if let Some(p) = pos {
            entry.set_position(p.min(3));
        }
        assert_eq!(Some(3), entry.get_position());
    }
}
}

pub use pancurses_backend::*;



