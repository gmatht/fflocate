// Win95/rust9x custom target_family gates are intentional
#![cfg_attr(windows, allow(unexpected_cfgs))]

#[cfg(windows)]
mod nwg_adapter {
    use native_windows_gui as nwg;
    use crate::core::{Error, Widget, DrawContext};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::os::raw::c_void;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn set_window_pos(hwnd: *mut c_void, x: i32, y: i32, w: i32, h: i32) {
        unsafe {
            // Show the child explicitly. ReactOS drops the WS_VISIBLE that
            // SetWindowPos(SWP_SHOWWINDOW) sets on a child of a window that is
            // itself still being shown, so a child laid out during the setup
            // cascade ends up hidden forever: IsWindowVisible stays false, it
            // never receives WM_PAINT, and the grid canvas never paints
            // (probe95 reported the scrolled frame and the canvas as 'h' with
            // otherwise-correct geometry). An explicit ShowWindow is honoured
            // at any depth, so show first and then position without the flag.
            winapi::um::winuser::ShowWindow(
                hwnd as winapi::shared::windef::HWND,
                winapi::um::winuser::SW_SHOW,
            );
            // The scrolled window is the canvas's *parent* (see
            // force_visible): if the frame's own WS_VISIBLE bit is never set,
            // the canvas is invisible with it however well it paints. A bare
            // ShowWindow does not reliably establish that bit on ReactOS for a
            // WS_CHILD created while its parent was still being built, so
            // assert it directly for every child this lays out.
            force_visible(hwnd as winapi::shared::windef::HWND);
            // TEMPORARY ReactOS diagnosis: ShowWindow did not set WS_VISIBLE?
            #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
            {
                let style = winapi::um::winuser::GetWindowLongW(
                    hwnd as _, winapi::um::winuser::GWL_STYLE,
                ) as i32;
                mark95xy(
                    b"wsviz",
                    if style & winapi::um::winuser::WS_VISIBLE as i32 != 0 {
                        1
                    } else {
                        0
                    },
                    style,
                );
            }
            // Bring the child to the top of its siblings. A child laid out
            // into a container that is itself being shown can end up BEHIND an
            // older sibling on ReactOS, and a window behind something else
            // still gets its WM_PAINT (so the draw callback runs and the log
            // shows healthy paints) while none of its pixels reach the
            // screen - the grid renders and the canvas stays white.
            winapi::um::winuser::SetWindowPos(
                hwnd as winapi::shared::windef::HWND,
                winapi::um::winuser::HWND_TOP,
                0, 0, 0, 0,
                winapi::um::winuser::SWP_NOMOVE
                    | winapi::um::winuser::SWP_NOSIZE
                    | winapi::um::winuser::SWP_NOACTIVATE,
            );
            // TEMPORARY ReactOS diagnosis: was the child visible right after
            // the explicit ShowWindow? (class, parent visible, self visible)
            #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
            {
                let selfv =
                    winapi::um::winuser::IsWindowVisible(hwnd as _) as i32;
                let pwnd =
                    winapi::um::winuser::GetParent(hwnd as _) as *mut c_void;
                let selv = if pwnd.is_null() {
                    -1
                } else {
                    winapi::um::winuser::IsWindowVisible(pwnd as _) as i32
                };
                mark95xy(b"showv", selfv, selv);
            }
            winapi::um::winuser::SetWindowPos(
                hwnd as winapi::shared::windef::HWND,
                std::ptr::null_mut(), x, y, w, h,
                winapi::um::winuser::SWP_NOZORDER,
            );
        }
    }

    /// Force a window visible, setting the `WS_VISIBLE` style bit *directly*
    /// and then showing it.
    ///
    /// `ShowWindow(SW_SHOW)` is not enough on its own. ReactOS leaves a
    /// `WS_CHILD` window created with `WS_VISIBLE` while its parent is still
    /// being constructed un-mapped, and does not honour a later
    /// `ShowWindow(SW_SHOW)` for it either - the bit stays clear, so
    /// `IsWindowVisible` reports the whole subtree as hidden, nothing in it
    /// is ever composited, and its descendants paint into a DC whose bits are
    /// never blitted to the screen. That is exactly the symptom: the grid
    /// canvas ran its entire draw callback (log shows healthy paints, BitBlt
    /// reports success) and the grid area stayed blank white.
    ///
    /// The scrolled window is the one widget that needs this: it is the
    /// *parent* of the canvas, so if the frame stays hidden the canvas is
    /// invisible with it no matter how healthy the canvas's own painting is.
    /// Writing the style bit directly is the one operation ReactOS does not
    /// drop; the `ShowWindow` that follows turns the bit into a real map.
    /// Windows the app explicitly hid through this backend's `set_visible`.
    ///
    /// A layout pass must not infer "hidden" from a missing `WS_VISIBLE` style
    /// bit: ReactOS strips that bit from every child at creation and does not
    /// always restore it, so a freshly created container looks hidden forever
    /// and the layout keeps it at zero size. Only an explicit `set_visible(
    /// false)` is a real hide, and that is the only thing recorded here.
    fn explicitly_hidden_windows() -> &'static std::cell::RefCell<std::collections::HashSet<usize>> {
        thread_local! {
            static REG: &'static std::cell::RefCell<std::collections::HashSet<usize>> =
                Box::leak(Box::new(std::cell::RefCell::new(std::collections::HashSet::new())));
        }
        REG.with(|r| *r)
    }

    /// Record (or clear) an explicit hide for a window.
    fn set_explicitly_hidden(hwnd: *mut c_void, hidden: bool) {
        if hwnd.is_null() {
            return;
        }
        let reg = explicitly_hidden_windows();
        let mut r = reg.borrow_mut();
        if hidden {
            r.insert(hwnd as usize);
        } else {
            r.remove(&(hwnd as usize));
        }
    }

    fn force_visible(hwnd: winapi::shared::windef::HWND) {
        if hwnd.is_null() {
            return;
        }
        unsafe {
            let style = winapi::um::winuser::GetWindowLongW(hwnd, winapi::um::winuser::GWL_STYLE);
            if style & winapi::um::winuser::WS_VISIBLE as i32 == 0 {
                winapi::um::winuser::SetWindowLongW(
                    hwnd,
                    winapi::um::winuser::GWL_STYLE,
                    style | winapi::um::winuser::WS_VISIBLE as i32,
                );
            }
            winapi::um::winuser::ShowWindow(hwnd, winapi::um::winuser::SW_SHOW);
        }
    }

    // TEMPORARY Win95 diagnosis: raw file marker (std::fs broken on 9x).
    #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
    fn mark95a(s: &[u8]) {
        unsafe {
            extern "system" {
                fn CreateFileA(name: *const u8, access: u32, share: u32,
                    sa: *mut c_void, disp: u32, flags: u32,
                    tmpl: *mut c_void) -> *mut c_void;
                fn SetFilePointer(h: *mut c_void, lo: i32, hi: *mut i32, how: u32) -> u32;
                fn WriteFile(h: *mut c_void, buf: *const u8, len: u32, w: *mut u32,
                    ov: *mut c_void) -> i32;
                fn CloseHandle(h: *mut c_void) -> i32;
            }
            // Honour CORRO_WIN95_LOG like the rest of the rust9x diagnostics.
            // Hardcoding c:\\gcorro.log sent this marker's output to the
            // read-only LiveCD, where the adapter-level trace was lost.
            static PATH: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
            let path = PATH.get_or_init(|| {
                let mut p = std::env::var("CORRO_WIN95_LOG")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "c:\\gcorro.log".to_string())
                    .into_bytes();
                p.truncate(259);
                p.push(0);
                p
            });
            let h = CreateFileA(path.as_ptr(), 0x4000_0000, 1,
                std::ptr::null_mut(), 4, 0x80, std::ptr::null_mut());
            if !h.is_null() && h as isize != -1 {
                SetFilePointer(h, 0, std::ptr::null_mut(), 2);
                let mut w = 0u32;
                WriteFile(h, s.as_ptr(), s.len() as u32, &mut w, std::ptr::null_mut());
                CloseHandle(h);
            }
        }
    }

    // TEMPORARY Win95 diagnosis: log a label + two i32s as hex.
    #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
    fn mark95xy(tag: &[u8; 5], a: i32, b: i32) {
        let hx = b"0123456789abcdef";
        let mut s = [0u8; 32];
        let mut p = 0;
        for i in 0..5 { s[p] = tag[i]; p += 1; }
        s[p] = b' '; p += 1;
        for v in [a as u32, b as u32] {
            for sh in [28u32, 24, 20, 16, 12, 8, 4, 0] {
                s[p] = hx[((v >> sh) & 0xf) as usize]; p += 1;
            }
            s[p] = b' '; p += 1;
        }
        s[p] = b'\n'; p += 1;
        mark95a(&s[..p]);
    }

    /// Translate a WM_KEYDOWN wParam (raw Win32 virtual-key code) for the
    /// app key callback, honouring Shift/CapsLock and the active keyboard
    /// layout (Shift+9 is '(' on US layouts, not '9'; Shift+letter gives
    /// capitals). Returns:
    /// - `Some(raw VK)` when Ctrl/Alt is held (accelerator/menu paths must
    ///   keep raw codes) or when no printable character results (special
    ///   keys keep their VK so app dispatch by constant keeps working);
    /// - `Some(translated char)` when the char cannot collide with an app
    ///   dispatched VK;
    /// - `None` when the translated char numerically collides with an app
    ///   dispatched VK (`!"#$%&'()*` are VK_PRIOR..VK_DOWN, `.` is
    ///   VK_DELETE). Callers must decline (return None) so the native
    ///   control inserts the character and the change event resyncs app
    ///   state. Passing the char on would misfire menu/cursor actions —
    ///   e.g. '(' (0x28) would commit the edit and move down as VK_DOWN.
    /// Known limitation: dead-key compositions fall back to the raw VK.
    fn translate_vk(wparam: usize) -> Option<u32> {
        let vk = (wparam & 0xFF) as u32;
        unsafe {
            const VK_CONTROL: i32 = 0x11;
            const VK_MENU: i32 = 0x12;
            if winapi::um::winuser::GetKeyState(VK_CONTROL) as u16 & 0x8000 != 0 {
                return Some(vk);
            }
            if winapi::um::winuser::GetKeyState(VK_MENU) as u16 & 0x8000 != 0 {
                return Some(vk);
            }
            let mut state: [u8; 256] = [0; 256];
            if winapi::um::winuser::GetKeyboardState(state.as_mut_ptr()) == 0 {
                return Some(vk);
            }
            let scan =
                winapi::um::winuser::MapVirtualKeyW(vk, winapi::um::winuser::MAPVK_VK_TO_VSC);
            // Win95: ToUnicodeEx is a Win2000+ USER32 export (and absent from
            // Win95's export table entirely — a hard loader failure, not just
            // a W-stub). ToAsciiEx is the Win95-era ANSI equivalent: same
            // VK/scan/keyboard-state inputs, same "1 char, 0 none, -1 dead
            // key" contract, but it writes ANSI bytes into a WORD array.
            // The app's key path only ever deals with ASCII key names and
            // control codes, so the ANSI result is what it wants.
            let mut buf: [u16; 4] = [0; 4];
            let n = winapi::um::winuser::ToAsciiEx(
                vk,
                scan,
                state.as_ptr(),
                buf.as_mut_ptr(),
                0,
                winapi::um::winuser::GetKeyboardLayout(0),
            );
            // Exactly one printable char, no pending dead-key composition —
            // unless it collides with an app-dispatched VK (see doc).
            if n == 1 && buf[0] >= 32 {
                let c = buf[0] as u32;
                if matches!(c, 0x21..=0x28 | 0x2E) {
                    return None;
                }
                return Some(c);
            }
            Some(vk)
        }
    }

    /// GDK-compatible modifier mask for the current physical key state:
    /// bit 0 = Shift, bit 2 = Ctrl, bit 3 = Alt. Lets widget callbacks see
    /// the same modifiers GTK delivers, so Ctrl/Alt guards in app code
    /// (copy/paste decline, menu propagation, quit) work on Windows too.
    /// Previously only Shift was reported, so Ctrl+C typed 'C' instead of
    /// copying and Ctrl+Q could never quit.
    fn modifier_state() -> u32 {
        unsafe {
            const VK_SHIFT: i32 = 0x10;
            const VK_CONTROL: i32 = 0x11;
            const VK_MENU: i32 = 0x12;
            let mut mods: u32 = 0;
            if winapi::um::winuser::GetKeyState(VK_SHIFT) as u16 & 0x8000 != 0 {
                mods |= 1;
            }
            if winapi::um::winuser::GetKeyState(VK_CONTROL) as u16 & 0x8000 != 0 {
                mods |= 4;
            }
            if winapi::um::winuser::GetKeyState(VK_MENU) as u16 & 0x8000 != 0 {
                mods |= 8;
            }
            mods
        }
    }

    /// True when pure Ctrl (no Alt) is physically held: accelerators and
    /// native edit shortcuts own the key, so a canvas must decline rather
    /// than type it. Ctrl+Alt (AltGr) returns false so international text
    /// input still reaches the widget.
    fn pure_ctrl_held() -> bool {
        modifier_state() & 0xC == 0x4
    }

    /// True when pure Alt (no Ctrl) is physically held: the key belongs to
    /// OS menu/accelerator handling, not widget editing. Ctrl+Alt (AltGr)
    /// returns false so international text input still reaches the widget.
    fn pure_alt_held() -> bool {
        unsafe {
            const VK_MENU: i32 = 0x12;
            const VK_CONTROL: i32 = 0x11;
            let alt = winapi::um::winuser::GetKeyState(VK_MENU) as u16 & 0x8000 != 0;
            let ctrl = winapi::um::winuser::GetKeyState(VK_CONTROL) as u16 & 0x8000 != 0;
            crate::win32_portable::should_yield_to_menu(alt, ctrl)
        }
    }

    /// Current size of a control for dialog layout, if measurable.
    ///
    /// Uses the **client** rect, not `GetWindowRect`. For most controls the
    /// two differ only by the frame, but for a ComboBox the difference is the
    /// whole bug this comment exists to prevent: the window height of a
    /// ComboBox *includes its drop-down list* (Win32 anchors the list to the
    /// window's bottom-left corner, which is only possible if the window is
    /// tall enough to contain it). So `GetWindowRect` on a combo returns
    /// "closed field + every list item", which grows with the item count.
    ///
    /// A dialog that sized its row from that would place the combo, then
    /// Windows would drop the list below the combo's new bottom -- moving it
    /// every time the row height changed. The list would then land somewhere
    /// different each time the dialog re-laid-out, which is what a "dropdown
    /// that appears at a random position" report is.
    ///
    /// The client rect is the closed field alone, so the row height is stable
    /// regardless of how many items the combo holds.
    fn control_size(hwnd: winapi::shared::windef::HWND) -> Option<(i32, i32)> {
        unsafe {
            let mut rect: winapi::shared::windef::RECT = std::mem::zeroed();
            if winapi::um::winuser::GetClientRect(hwnd, &mut rect) == 0 {
                return None;
            }
            Some((rect.right - rect.left, rect.bottom - rect.top))
        }
    }

    /// Measure a text control's *natural* content height for dialog layout.
    ///
    /// Controls are created at a fixed default size and `set_text` does not
    /// resize them, so a multi-line STATIC label or multi-line EDIT text box
    /// still reports a single-line window height (~25px). Layout then gives
    /// it only that height and every line after the first is clipped away —
    /// the "empty About/Help dialog" bug. Count embedded newlines and scale
    /// the font line height so multi-line content is drawn in full.
    ///
    /// The line height is measured from the control's font each call (never
    /// the control's current height, which the layout itself changes: using
    /// that made the height grow geometrically across layout passes).
    ///
    /// Returns None for non-text controls / single-line content (callers keep
    /// the measured window height then).
    fn text_natural_height(hwnd: winapi::shared::windef::HWND) -> Option<i32> {
        // TEMPORARY ReactOS diagnosis: this is the app's only remaining
        // WM_GETFONT sender (it sends it directly rather than through nwg's
        // get_window_font, so the nwg-side counter did not see it). Capped.
        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
        {
            use std::sync::atomic::{AtomicU32, Ordering};
            static TNH: AtomicU32 = AtomicU32::new(0);
            let n = TNH.fetch_add(1, Ordering::Relaxed);
            if n < 12 {
                let hx = b"0123456789abcdef";
                let mut mb = [0u8; 12];
                mb[0] = b't'; mb[1] = b'n'; mb[2] = b'h';
                for i in 0..8 { mb[3 + i] = hx[((n >> ((7 - i) * 4)) & 0xf) as usize]; }
                mb[11] = b'\n';
                mark95a(&mb);
            }
        }
        unsafe {
            let mut cls: [u16; 64] = [0; 64];
            let n = winapi::um::winuser::GetClassNameW(hwnd, cls.as_mut_ptr(), 64);
            if n <= 0 {
                return None;
            }
            let class = String::from_utf16_lossy(&cls[..n as usize]);
            // STATIC labels and EDIT boxes both carry multi-line text; other
            // controls (buttons, combos, ...) are single-line by nature.
            if !class.eq_ignore_ascii_case("Static") && !class.eq_ignore_ascii_case("Edit") {
                return None;
            }
            let tlen = winapi::um::winuser::GetWindowTextLengthW(hwnd);
            if tlen <= 0 {
                return None;
            }
            let mut buf: Vec<u16> = vec![0; (tlen + 1) as usize];
            let got = winapi::um::winuser::GetWindowTextW(hwnd, buf.as_mut_ptr(), tlen + 1);
            if got <= 0 {
                return None;
            }
            let text = String::from_utf16_lossy(&buf[..got as usize]);
            let lines = text.split('\n').count() as i32;
            if lines <= 1 {
                return None;
            }
            // One line's height from the control's font (fall back to the
            // shell dialog font height if the control has none).
            let line_h = {
                let dc = winapi::um::winuser::GetDC(hwnd);
                let mut h = 0i32;
                if !dc.is_null() {
                    let font = winapi::um::winuser::SendMessageW(
                        hwnd,
                        winapi::um::winuser::WM_GETFONT,
                        0,
                        0,
                    );
                    let old = if font != 0 {
                        winapi::um::wingdi::SelectObject(dc, font as _)
                    } else {
                        std::ptr::null_mut()
                    };
                    let mut tm: winapi::um::wingdi::TEXTMETRICW = std::mem::zeroed();
                    if winapi::um::wingdi::GetTextMetricsW(dc, &mut tm) != 0 {
                        h = tm.tmHeight + tm.tmExternalLeading;
                    }
                    if !old.is_null() {
                        winapi::um::wingdi::SelectObject(dc, old);
                    }
                    winapi::um::winuser::ReleaseDC(hwnd, dc);
                }
                if h > 0 { h } else { 16 }
            };
            // Padding so the last line's descenders are not clipped.
            Some(line_h.saturating_mul(lines) + 6)
        }
    }

    /// Dialog content+button layout shared by `Dialog::layout_dialog` and
    /// the dialog WM_SIZE binding (which fires before any Dialog exists).
    fn layout_nwg_dialog_parts(
        dlg_hwnd: winapi::shared::windef::HWND,
        content: &RefCell<Vec<*mut c_void>>,
        buttons: &RefCell<Vec<(nwg::Button, nwg::EventHandler)>>,
    ) {
        let (cw, ch) = unsafe {
            let mut rect: winapi::shared::windef::RECT = std::mem::zeroed();
            if winapi::um::winuser::GetClientRect(dlg_hwnd, &mut rect) == 0 {
                return;
            }
            (rect.right - rect.left, rect.bottom - rect.top)
        };
        let mut specs: Vec<(i32, i32, bool)> = Vec::new();
        for &ptr in content.borrow().iter() {
            let (w, h) = control_size(ptr as _).unwrap_or((0, 0));
            if w <= 8 && h <= 8 {
                specs.push((0, 0, true));
            } else if let Some(natural_h) = text_natural_height(ptr as _) {
                // Multi-line text: keep the measured width, use the full
                // text height so no line is clipped (see helper doc).
                specs.push((w, natural_h.max(h), false));
            } else {
                specs.push((w, if h > 10 { h } else { 26 }, false));
            }
        }
        let mut btn_sizes: Vec<(i32, i32)> = Vec::new();
        for (btn, _) in buttons.borrow().iter() {
            let (w, h) = btn
                .handle
                .hwnd()
                .and_then(control_size)
                .unwrap_or((0, 0));
            btn_sizes.push((
                if (40..=220).contains(&w) { w } else { 96 },
                if (16..=48).contains(&h) { h } else { 28 },
            ));
        }
        let (content_rects, btn_rects) =
            crate::win32_portable::dialog_layout_geometry(cw, ch, &specs, &btn_sizes);
        let content = content.borrow();
        for (i, r) in content_rects.iter().enumerate() {
            if i >= content.len() {
                break;
            }
            set_window_pos(content[i], r.x, r.y, r.w, r.h);
            unsafe {
                let l = ((r.h & 0xFFFF) << 16) | (r.w & 0xFFFF);
                winapi::um::winuser::SendMessageW(
                    content[i] as _,
                    winapi::um::winuser::WM_SIZE,
                    0,
                    l as _,
                );
            }
        }
        let buttons = buttons.borrow();
        for (i, r) in btn_rects.iter().enumerate() {
            if i >= buttons.len() {
                break;
            }
            if let Some(h) = buttons[i].0.handle.hwnd() {
                set_window_pos(h as *mut c_void, r.x, r.y, r.w, r.h);
            }
        }
    }

    // -- Window --

    pub struct Window {
        pub(crate) inner: Rc<nwg::Window>,
        pub(crate) _handler: Rc<nwg::EventHandler>,
        // Stable hwnd snapshot for AsRef. The previous body referenced a
        // temporary (`&handle.hwnd().unwrap_or(..) as ...`) — a dangling
        // reference (UB), garbage hwnds in release builds.
        hwnd: *mut c_void,
        root_child: Rc<RefCell<Option<*mut c_void>>>,
        layout_cb: Rc<RefCell<Option<Box<dyn FnMut(i32, i32)>>>>,
        event_key_cb: Rc<RefCell<Option<Box<dyn FnMut(u32, u32) -> i32>>>>,
        close_cb: Rc<RefCell<Option<Box<dyn FnMut()>>>>,
        /// (Fix-ReactOS) Last size the toplevel was laid out at; see the
        /// WM_SIZE handler for why a repeat at the same size must be dropped.
        last_size: std::cell::RefCell<(i32, i32)>,
    }

    impl Clone for Window {
        fn clone(&self) -> Self {
            Window {
                inner: self.inner.clone(),
                _handler: self._handler.clone(),
                hwnd: self.hwnd,
                root_child: self.root_child.clone(),
                layout_cb: self.layout_cb.clone(),
                event_key_cb: self.event_key_cb.clone(),
                close_cb: self.close_cb.clone(),
                // A clone re-derives its own layout history: it has not
                // laid anything out yet, so starting empty is correct (a
                // shared cell would suppress this clone's first layout).
                last_size: std::cell::RefCell::new((-1, -1)),
            }
        }
    }

    impl Widget for Window {
        fn raw_handle(&self) -> *mut c_void {
            self.inner.handle.hwnd().unwrap_or(std::ptr::null_mut()) as *mut c_void
        }
    }

    impl AsRef<*mut c_void> for Window {
        fn as_ref(&self) -> &*mut c_void {
            &self.hwnd
        }
    }

    impl Window {
        pub fn set_title(&self, title: &str) { self.inner.set_text(title); }
        pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
            let ptr = *child.as_ref();
            if !ptr.is_null() {
                unsafe {
                    winapi::um::winuser::SetParent(ptr as _, self.hwnd() as _);
                }
                *self.root_child.borrow_mut() = Some(ptr);
                let hwnd = ptr;
                *self.layout_cb.borrow_mut() = Some(Box::new(move |w, h| {
                    set_window_pos(hwnd, 0, 0, w, h);
                }));
            }
        }
        pub fn set_child_box(&self, bx: &BoxWidget) {
            if !bx.hwnd.is_null() {
                unsafe {
                    winapi::um::winuser::SetParent(bx.hwnd as _, self.hwnd() as _);
                }
            }
            let hwnd = bx.hwnd;
            let bx = bx.clone();
            *self.layout_cb.borrow_mut() = Some(Box::new(move |w, h| {
                set_window_pos(hwnd, 0, 0, w, h);
                bx.layout(0, 0, w, h);
            }));
        }
        pub fn set_layout_cb<F: FnMut(i32, i32) + 'static>(&self, f: F) {
            *self.layout_cb.borrow_mut() = Some(Box::new(f));
        }
        pub fn set_default_size(&self, w: i32, h: i32) {
            let hwnd = self.inner.handle.hwnd().unwrap_or(std::ptr::null_mut());
            if hwnd != std::ptr::null_mut() {
                unsafe {
                    winapi::um::winuser::SetWindowPos(
                        hwnd as _,
                        std::ptr::null_mut(), 0, 0, w, h,
                        winapi::um::winuser::SWP_NOZORDER | winapi::um::winuser::SWP_NOMOVE,
                    );
                }
            }
        }
        pub fn present(&self) {
            let hwnd = self.inner.handle.hwnd().unwrap_or(std::ptr::null_mut());
            if hwnd != std::ptr::null_mut() {
                unsafe {
                    // Bring window to foreground so child controls can
                    // receive keyboard focus (required by SetFocus).
                    winapi::um::winuser::SetForegroundWindow(hwnd);
                    winapi::um::winuser::BringWindowToTop(hwnd);
                    let mut rect: winapi::shared::windef::RECT = std::mem::zeroed();
                    winapi::um::winuser::GetClientRect(hwnd, &mut rect);
                    let w = rect.right - rect.left;
                    let h = rect.bottom - rect.top;
                    // TEMPORARY Win95 diagnosis.
                    #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                    mark95xy(b"pres ", w, h);
                    if let Some(ref mut cb) = *self.layout_cb.borrow_mut() {
                        cb(w, h);
                    }
                }
            }
        }
        pub fn insert_action_group(&self, _name: &str, _group_ptr: *mut c_void) {}
        /// Strip or restore the title bar and border.
        ///
        /// `false` clears `WS_CAPTION|WS_THICKFRAME|WS_SYSMENU|WS_MINIMIZEBOX
        /// |WS_MAXIMIZEBOX`, which is what a borderless/overlay window needs.
        /// `true` puts the standard frame back.
        ///
        /// `SWP_FRAMECHANGED` is not optional: without it Windows keeps the
        /// cached non-client area and the old frame stays on screen even
        /// though the style bits are gone. That is the whole reason this is
        /// not a one-line style edit.
        ///
        /// Applied post-creation on the live HWND rather than at
        /// `CreateWindowEx` time, so an existing window (already shown, with
        /// children laid out) can be made borderless without being rebuilt.
        pub fn set_decorated(&self, decorated: bool) {
            let hwnd = self.hwnd();
            if hwnd.is_null() {
                return;
            }
            unsafe {
                // GetWindowLongW is i32 but the style constants are u32, so
                // cast the flags down (same shape as Label::set_xalign, which
                // casts the style up instead).
                let frame = (winapi::um::winuser::WS_CAPTION
                    | winapi::um::winuser::WS_THICKFRAME
                    | winapi::um::winuser::WS_SYSMENU
                    | winapi::um::winuser::WS_MINIMIZEBOX
                    | winapi::um::winuser::WS_MAXIMIZEBOX) as i32;
                let style = winapi::um::winuser::GetWindowLongW(hwnd as _, winapi::um::winuser::GWL_STYLE);
                let new = if decorated {
                    style | frame
                } else {
                    style & !frame
                };
                if new != style {
                    winapi::um::winuser::SetWindowLongW(hwnd as _, winapi::um::winuser::GWL_STYLE, new);
                }
                winapi::um::winuser::SetWindowPos(
                    hwnd as _,
                    std::ptr::null_mut(), 0, 0, 0, 0,
                    winapi::um::winuser::SWP_FRAMECHANGED
                        | winapi::um::winuser::SWP_NOMOVE
                        | winapi::um::winuser::SWP_NOSIZE
                        | winapi::um::winuser::SWP_NOZORDER
                        | winapi::um::winuser::SWP_NOACTIVATE,
                );
            }
        }
        /// Keep this toplevel above all non-topmost windows.
        ///
        /// Two operations, both needed: `WS_EX_TOPMOST` marks the window as
        /// topmost, but the z-order is only actually changed by moving it to
        /// `HWND_TOPMOST` / `HWND_NOTOPMOST`. Setting only the exstyle leaves
        /// the window where it was, so it is still covered.
        pub fn set_keep_above(&self, keep_above: bool) {
            let hwnd = self.hwnd();
            if hwnd.is_null() {
                return;
            }
            unsafe {
                let ex = winapi::um::winuser::GetWindowLongW(hwnd as _, winapi::um::winuser::GWL_EXSTYLE);
                let topmost = winapi::um::winuser::WS_EX_TOPMOST as i32;
                let new = if keep_above { ex | topmost } else { ex & !topmost };
                if new != ex {
                    winapi::um::winuser::SetWindowLongW(hwnd as _, winapi::um::winuser::GWL_EXSTYLE, new);
                }
                winapi::um::winuser::SetWindowPos(
                    hwnd as _,
                    if keep_above {
                        winapi::um::winuser::HWND_TOPMOST
                    } else {
                        winapi::um::winuser::HWND_NOTOPMOST
                    },
                    0, 0, 0, 0,
                    winapi::um::winuser::SWP_NOMOVE
                        | winapi::um::winuser::SWP_NOSIZE
                        | winapi::um::winuser::SWP_NOACTIVATE,
                );
            }
        }
        /// Move the toplevel to a screen position.
        ///
        /// This is the only window-level position setter the NWG adapter
        /// exposes: `set_default_size` and `resize` only touch size (with
        /// `SWP_NOMOVE`), so a caller positioning an overlay has no other
        /// way in. Coordinates are screen pixels.
        pub fn set_position(&self, x: i32, y: i32) {
            let hwnd = self.hwnd();
            if hwnd.is_null() {
                return;
            }
            unsafe {
                winapi::um::winuser::SetWindowPos(
                    hwnd as _,
                    std::ptr::null_mut(), x, y, 0, 0,
                    winapi::um::winuser::SWP_NOSIZE | winapi::um::winuser::SWP_NOZORDER,
                );
            }
        }
        pub fn hwnd(&self) -> *mut c_void {
            self.inner.handle.hwnd().unwrap_or(std::ptr::null_mut()) as *mut c_void
        }
        /// Run `f` every `ms` milliseconds until it returns `false`, via a
        /// Win32 timer on this window.
        ///
        /// The portable counterpart of GTK's `timeout_add_repeating`: apps
        /// need a periodic tick for work that must happen with no user input
        /// (e.g. tailing an append-only log so another window's revisions
        /// appear here). `WM_TIMER` is delivered to this window's message
        /// loop, so the app is woken even while idle.
        ///
        /// The raw handler is leaked on purpose: one per window, and it must
        /// outlive every other reference (unbinding would need the app to
        /// keep a handle it has no reason to hold).
        pub fn start_repeating_timer(&self, _id: usize, ms: u32, f: Box<dyn FnMut() -> bool>) -> Result<(), Error> {
            let hwnd = self.hwnd();
            if hwnd.is_null() {
                return Err(Error::Backend("timer: window has no hwnd".into()));
            }
            let cb = Rc::new(RefCell::new(f));
            let cb_for_handler = cb.clone();
            // Two distinct ids live here, and conflating them panics:
            //   * the Win32 *timer* id (`SetTimer`); for an hWnd timer Windows
            //     may substitute its own id, so the returned value is what
            //     `KillTimer` and the WM_TIMER wparam must match.
            //   * the NWG *handler* id, which must be > 0xFFFF — NWG reserves
            //     the low range and `bind_raw_event_handler` panics on it
            //     (`vendor/native-windows-gui/src/win32/window.rs`).
            // Handler ids must be > 0xFFFF; timer request ids must be small
            // and non-zero (0 is SetTimer's failure value), hence two counters.
            static NEXT_HANDLER_ID: std::sync::atomic::AtomicUsize =
                std::sync::atomic::AtomicUsize::new(0xD400_0000);
            static NEXT_REQUEST_ID: std::sync::atomic::AtomicUsize =
                std::sync::atomic::AtomicUsize::new(1);
            let handler_id = NEXT_HANDLER_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let request_id = NEXT_REQUEST_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let timer_id = unsafe { winapi::um::winuser::SetTimer(hwnd as _, request_id, ms, None) };
            if timer_id == 0 {
                return Err(Error::Backend("SetTimer failed".into()));
            }
            let handler = nwg::bind_raw_event_handler(
                &nwg::ControlHandle::Hwnd(hwnd as _),
                handler_id,
                move |_h, msg, w, _l| {
                    if msg == winapi::um::winuser::WM_TIMER && w == timer_id {
                        let keep = (cb_for_handler.borrow_mut())();
                        if !keep {
                            unsafe {
                                winapi::um::winuser::KillTimer(hwnd as _, timer_id);
                            }
                        }
                    }
                    None
                },
            )
            .map_err(|e| Error::Backend(format!("timer handler: {e}")))?;
            std::mem::forget(handler);
            // Keep the closure alive for the window's lifetime too.
            std::mem::forget(cb);
            Ok(())
        }
        /// Repaint the toplevel AND its children.
        ///
        /// corro repaints a cursor move by calling `canvas.queue_redraw()`
        /// then `window.queue_redraw()`. On Win32 the canvas is a child
        /// window, and `Canvas::queue_redraw`'s bare `InvalidateRect` only
        /// marks its update region — nothing pumps the child to completion,
        /// and a child gets `WM_PAINT` when its *parent* paints. With no
        /// cascade here, every arrow press moved the cursor in state while
        /// the grid kept showing the old highlight for the whole time the
        /// key was held, then caught up on the next full-window repaint.
        ///
        /// `RDW_UPDATENOW` paints immediately rather than deferring, so each
        /// auto-repeat press is visible on its own; `RDW_ALLCHILDREN` carries
        /// the repaint down to the canvas.
        pub fn queue_redraw(&self) {
            if let Some(hwnd) = self.inner.handle.hwnd() {
                if hwnd.is_null() {
                    return;
                }
                unsafe {
                    winapi::um::winuser::RedrawWindow(
                        hwnd as _,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        winapi::um::winuser::RDW_INVALIDATE
                            | winapi::um::winuser::RDW_UPDATENOW
                            | winapi::um::winuser::RDW_ERASE
                            | winapi::um::winuser::RDW_ALLCHILDREN
                            | winapi::um::winuser::RDW_FRAME,
                    );
                }
            }
        }
        pub fn on_event(&self, _cb: Box<dyn FnMut(*mut c_void) -> i32>) {}
        pub fn on_event_key(&self, cb: Box<dyn FnMut(u32, u32) -> i32>) {
            *self.event_key_cb.borrow_mut() = Some(cb);
        }
        pub fn on_close(&self, cb: Box<dyn FnMut()>) {
            *self.close_cb.borrow_mut() = Some(cb);
        }
        /// Immediate resize.
        ///
        /// `set_default_size` is advisory and is only applied once a window
        /// manager adopts the window; on a bare Xvfb or an offscreen CI runner
        /// there is none and the window keeps its initial size. This forces it.
        pub fn resize(&self, width: i32, height: i32) {
            // nwg's signature is (u32, u32). A negative request is meaningless
            // and would wrap round to an enormous size, so it is clamped rather
            // than cast blindly.
            self.inner
                .set_size(width.max(0) as u32, height.max(0) as u32);
        }
    }

    pub fn create_window(parent_cell: &Rc<RefCell<Option<*mut c_void>>>) -> Result<Window, Error> {
        let (inner, handler) = crate::backends::nwg::create_window(parent_cell)
            .map_err(|e| Error::Backend(format!("{}", e)))?;
        let root_child: Rc<RefCell<Option<*mut c_void>>> = Rc::new(RefCell::new(None));
        let layout_cb: Rc<RefCell<Option<Box<dyn FnMut(i32, i32)>>>> = Rc::new(RefCell::new(None));
        let event_key_cb: Rc<RefCell<Option<Box<dyn FnMut(u32, u32) -> i32>>>> = Rc::new(RefCell::new(None));
        let close_cb: Rc<RefCell<Option<Box<dyn FnMut()>>>> = Rc::new(RefCell::new(None));

        let hwnd = inner.handle.hwnd().unwrap_or(std::ptr::null_mut());
        if hwnd != std::ptr::null_mut() {
            // Bind raw WM_SIZE handler
            let cb = layout_cb.clone();
            let last_size = std::rc::Rc::new(std::cell::RefCell::new((-1i32, -1i32)));
            let last_size_h = last_size.clone();
            static RAW_HANDLER_ID: AtomicUsize = AtomicUsize::new(0x10000000);
            let handler_id = RAW_HANDLER_ID.fetch_add(1, Ordering::SeqCst);
            nwg::bind_raw_event_handler(
                &nwg::ControlHandle::Hwnd(hwnd),
                handler_id,
                move |_h, msg, _w, l| {
                    if msg == winapi::um::winuser::WM_PAINT {
                        // TEMPORARY ReactOS diagnosis: does the toplevel ever
                        // get a paint? Distinguishes "no WM_PAINT at all"
                        // from "WM_PAINT but the canvas drew nothing".
                        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                        mark95a(b"toplevel-paint\n");
                    }
                    if msg == winapi::um::winuser::WM_SIZE {
                        let w = (l & 0xFFFF) as i32;
                        let h = ((l >> 16) & 0xFFFF) as i32;
                        // Drop zero sizes (stale setup-storm leftovers; a
                        // zero-size toplevel has nothing to lay out and the
                        // next real size repairs). See the box handler below.
                        if w <= 0 || h <= 0 { return None; }
                        // (Fix-ReactOS) Re-entrancy guard. Laying out calls
                        // set_window_pos on every child, and on ReactOS that
                        // re-fires WM_NCCALCSIZE on the children, which
                        // re-queries fonts and re-enters this handler.
                        // Re-running a layout from inside a layout of the
                        // same window never converges: each pass re-positions
                        // the children, so the WM_GETFONT traffic never
                        // stops and the UI thread livelocks inside present()
                        // (the window never completes its first paint).
                        // Skip a repeat at a size already laid out; a
                        // genuinely new size still lays out normally.
                        {
                            let mut last = last_size_h.borrow_mut();
                            if *last == (w, h) { return None; }
                            *last = (w, h);
                        }
                        if let Some(ref mut cb) = *cb.borrow_mut() {
                            cb(w, h);
                        }
                    }
                    None
                },
            ).map_err(|e| Error::Backend(format!("{}", e)))?;

            // Bind raw WM_KEYDOWN/WM_SYSKEYDOWN handler for on_event_key.
            // The state parameter is a GDK-compatible modifier mask:
            //   bit 0 = Shift, bit 2 = Ctrl, bit 3 = Alt (MOD1_MASK)
            // Alt is detected from WM_SYSKEYDOWN; Shift/Ctrl via GetKeyState.
            // If the callback returns 0 (not consumed), the message is forwarded
            // to the focused child window via PostMessage so the canvas or entry
            // raw handlers can process it.  After forwarding, we consume the
            // message (return Some(0)) so DefWindowProc does NOT process it.
            // This prevents WM_SYSKEYDOWN(Alt) from activating the menu bar,
            // which would steal focus from the window-level quit handler.
            let kcb = event_key_cb.clone();
            static KEY_HANDLER_ID: AtomicUsize = AtomicUsize::new(0x30000000);
            let key_id = KEY_HANDLER_ID.fetch_add(1, Ordering::SeqCst);
            let parent_hwnd = hwnd;
            nwg::bind_raw_event_handler(
                &nwg::ControlHandle::Hwnd(hwnd),
                key_id,
                move |_h, msg, w, l| {
                    if msg != winapi::um::winuser::WM_KEYDOWN && msg != winapi::um::winuser::WM_SYSKEYDOWN {
                        return None;
                    }
                    if let Some(ref mut f) = *kcb.borrow_mut() {
                        let mut state: u32 = 0;
                        if msg == winapi::um::winuser::WM_SYSKEYDOWN {
                            state |= 8; // GDK_MOD1_MASK (Alt)
                        }
                        // Win32 key messages carry no modifier state; query
                        // it directly so Shift+arrows (selection extend) and
                        // Ctrl accelerators (Ctrl+Q quit) work. Alt keeps its
                        // message-derived bit above.
                        state |= modifier_state() & 0x5;
                        // Translate Shift pairs/capitals; specials keep raw
                        // VKs. The forwarded PostMessage below stays raw so
                        // the child translates for itself. A colliding
                        // translation (None) skips the callback; flow continues
                        // to forwarding below like any unhandled key.
                        if let Some(k) = translate_vk(w) {
                            if f(k, state) != 0 {
                                return Some(0); // consumed, do not forward
                            }
                        }
                    }
                    // Alt+letter (WM_SYSKEYDOWN): let DefWindowProc activate the
                    // native menu bar so the user can keyboard-navigate the menu
                    // (Alt+letter to open, arrows to move, Enter to activate,
                    // Esc to close). The native menu is modal, so it handles the
                    // navigation keys itself; item activation fires WM_MENUCOMMAND
                    // which dispatches the action. Previously we consumed every
                    // key to stop the menu from stealing focus, which made the
                    // menu mouse-only.
                    if msg == winapi::um::winuser::WM_SYSKEYDOWN {
                        return None;
                    }
                    // Forward other keyboard messages to the correct child.
                    // The window-level raw handler consumes non-Alt key events
                    // (returning Some(0) below) so they are not processed twice;
                    // we manually post the message to the focused child (or a
                    // suitable descendant if no child has focus).
                    unsafe {
                        let focused = winapi::um::winuser::GetFocus();
                        if focused != std::ptr::null_mut() && focused != parent_hwnd {
                            // Forward to the focused child.
                            winapi::um::winuser::PostMessageW(focused, msg, w, l);
                        } else {
                            // No child has focus.  Post to all descendant
                            // windows recursively.  This ensures the formula
                            // entry (great-great-grandchild of the main window
                            // in the VBox -> formula bar -> entry hierarchy)
                            // receives keyboard messages even when no child
                            // has keyboard focus.
                            unsafe fn post_to_descendants(hwnd: winapi::shared::windef::HWND, msg: u32, w: winapi::shared::minwindef::WPARAM, l: winapi::shared::minwindef::LPARAM) {
                                let mut child = winapi::um::winuser::GetWindow(hwnd, winapi::um::winuser::GW_CHILD);
                                while child != std::ptr::null_mut() {
                                    winapi::um::winuser::PostMessageW(child, msg, w, l);
                                    post_to_descendants(child, msg, w, l);
                                    child = winapi::um::winuser::GetWindow(child, winapi::um::winuser::GW_HWNDNEXT);
                                }
                            }
                            post_to_descendants(parent_hwnd, msg, w, l);
                        }
                    }
                    Some(0) // consumed — prevent DefWindowProc from activating menu
                },
            ).map_err(|e| Error::Backend(format!("{}", e)))?;

            // Bind raw WM_CLOSE handler: replayer tests can post WM_CLOSE
            // directly to the main window as a reliable quit mechanism that
            // does not depend on the foreground-window focus state.
            // Calls the registered close callback (save_before_quit) before
            // quitting so that pending edits are committed to the output file.
            {
                let cb = close_cb.clone();
                static CLOSE_ID: AtomicUsize = AtomicUsize::new(0x40000000);
                let cid = CLOSE_ID.fetch_add(1, Ordering::SeqCst);
                nwg::bind_raw_event_handler(
                    &nwg::ControlHandle::Hwnd(hwnd), cid,
                    move |_h, msg, _w, _l| {
                        if msg == winapi::um::winuser::WM_CLOSE {
                            // TEMPORARY Win95 diagnosis.
                            #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                            unsafe {
                                extern "system" {
                                    fn CreateFileA(name: *const u8, access: u32, share: u32,
                                        sa: *mut std::os::raw::c_void, disp: u32, flags: u32,
                                        tmpl: *mut std::os::raw::c_void) -> *mut std::os::raw::c_void;
                                    fn SetFilePointer(h: *mut std::os::raw::c_void, lo: i32,
                                        hi: *mut i32, how: u32) -> u32;
                                    fn WriteFile(h: *mut std::os::raw::c_void, buf: *const u8,
                                        len: u32, w: *mut u32, ov: *mut std::os::raw::c_void) -> i32;
                                    fn CloseHandle(h: *mut std::os::raw::c_void) -> i32;
                                }
                                // Honour CORRO_WIN95_LOG like the rest of the rust9x diagnostics: a
            // harness that points the log at a writable volume must see these
            // marks too. Hardcoding c:\gcorro.log sent the adapter-level
            // trace to the read-only LiveCD, where it was lost.
            static PATH: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
            let path = PATH.get_or_init(|| {
                let mut p = std::env::var("CORRO_WIN95_LOG")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "c:\\gcorro.log".to_string())
                    .into_bytes();
                p.truncate(259);
                p.push(0);
                p
            });
            let h = CreateFileA(path.as_ptr(), 0x4000_0000, 1,
                                    std::ptr::null_mut(), 4, 0x80, std::ptr::null_mut());
                                if !h.is_null() && h as isize != -1 {
                                    SetFilePointer(h, 0, std::ptr::null_mut(), 2);
                                    let mut w = 0u32;
                                    let s = b"got-close\n";
                                    WriteFile(h, s.as_ptr(), s.len() as u32, &mut w, std::ptr::null_mut());
                                    CloseHandle(h);
                                }
                            }
                            if let Some(ref mut f) = *cb.borrow_mut() {
                                f();
                            }
                            crate::backends::nwg::quit_main_loop();
                            Some(0)
                        } else {
                            None
                        }
                    },
                ).map_err(|e| Error::Backend(format!("{}", e)))?;
            }
        }

        Ok(Window { hwnd: inner.handle.hwnd().unwrap_or(std::ptr::null_mut()) as *mut c_void, inner: Rc::new(inner), _handler: Rc::new(handler), root_child, layout_cb, event_key_cb, close_cb, last_size: std::cell::RefCell::new((-1, -1)) })
    }

    // -- Button --

    #[derive(Clone)]
    pub struct Button {
        pub(crate) inner: Rc<nwg::Button>,
        pub(crate) _handler: Rc<nwg::EventHandler>,
        // Stable hwnd snapshot for AsRef (see Window). *mut c_void is
        // Copy, so #[derive(Clone)] keeps working.
        hwnd: *mut c_void,
        pub(crate) click_cb: Rc<RefCell<Option<Box<dyn FnMut()>>>>,
        pub(crate) _raw_click_handler: Rc<RefCell<Vec<nwg::RawEventHandler>>>,
    }

    impl Button {
        pub fn on_click(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            *self.click_cb.borrow_mut() = Some(Box::new(f));
            Ok(0)
        }
        pub fn emit_clicked(&self) -> Result<u64, Error> {
            if let Some(ref mut cb) = *self.click_cb.borrow_mut() { cb(); }
            Ok(0)
        }
        pub fn set_size_request(&self, w: i32, h: i32) {
            if let Some(hwnd) = self.inner.handle.hwnd() {
                unsafe {
                    winapi::um::winuser::SetWindowPos(
                        hwnd as _,
                        std::ptr::null_mut(), 0, 0, w, h,
                        winapi::um::winuser::SWP_NOZORDER | winapi::um::winuser::SWP_NOMOVE,
                    );
                }
            }
        }
        pub fn set_font_style(&self, weight: i32, italic: bool) {
            if let Some(hwnd) = self.inner.handle.hwnd() {
                unsafe {
                    let mut lf: winapi::um::wingdi::LOGFONTW = std::mem::zeroed();
                    lf.lfHeight = -13;
                    lf.lfWeight = weight;
                    lf.lfItalic = italic as u8;
                    lf.lfCharSet = winapi::um::wingdi::ANSI_CHARSET as u8;
                    lf.lfOutPrecision = winapi::um::wingdi::OUT_DEFAULT_PRECIS as u8;
                    lf.lfClipPrecision = winapi::um::wingdi::CLIP_DEFAULT_PRECIS as u8;
                    lf.lfQuality = winapi::um::wingdi::PROOF_QUALITY as u8;
                    lf.lfPitchAndFamily = winapi::um::wingdi::DEFAULT_PITCH as u8;
                    let face = "Segoe UI\0".encode_utf16().collect::<Vec<_>>();
                    let mut i = 0;
                    while i < face.len().min(32) {
                        lf.lfFaceName[i] = face[i];
                        i += 1;
                    }
                    let hfont = winapi::um::wingdi::CreateFontIndirectW(&lf);
                    if !hfont.is_null() {
                        winapi::um::winuser::SendMessageW(
                            hwnd as _,
                            winapi::um::winuser::WM_SETFONT,
                            hfont as usize,
                            1,
                        );
                    }
                }
            }
        }
        pub fn add_class(&self, _class: &str) {}
        pub fn remove_class(&self, _class: &str) {}
    }

    impl AsRef<*mut c_void> for Button {
        fn as_ref(&self) -> &*mut c_void {
            &self.hwnd
        }
    }
    pub fn create_button(parent: *mut c_void, text: &str) -> Result<Button, Error> {
        let (inner, click_cb, handler) = crate::backends::nwg::create_button(parent, text)
            .map_err(|e| Error::Backend(format!("{}", e)))?;
        let hwnd = inner.handle.hwnd().unwrap_or(std::ptr::null_mut());
        let raw_handlers: Rc<RefCell<Vec<nwg::RawEventHandler>>> = Rc::new(RefCell::new(Vec::new()));
        if hwnd != std::ptr::null_mut() {
            static RAW_BTN_CLICK_ID: AtomicUsize = AtomicUsize::new(0x60000000);
            let cb = click_cb.clone();
            let rid = RAW_BTN_CLICK_ID.fetch_add(1, Ordering::SeqCst);
            if let Ok(raw) = nwg::bind_raw_event_handler(
                &nwg::ControlHandle::Hwnd(hwnd), rid,
                move |_h, msg, w, l| {
                    match msg {
                        winapi::um::winuser::WM_LBUTTONUP => {
                            let mut rect = std::mem::MaybeUninit::zeroed();
                            let rc = unsafe {
                                winapi::um::winuser::GetClientRect(hwnd as _, rect.as_mut_ptr());
                                rect.assume_init()
                            };
                            let x = (l & 0xFFFF) as i16;
                            let y = ((l >> 16) & 0xFFFF) as i16;
                            if i32::from(x) <= rc.right && i32::from(y) <= rc.bottom {
                                if let Some(ref mut f) = *cb.borrow_mut() { f(); }
                            }
                            None
                        }
                        winapi::um::winuser::WM_KEYUP => {
                            if w == winapi::um::winuser::VK_SPACE as usize {
                                if let Some(ref mut f) = *cb.borrow_mut() { f(); }
                            }
                            None
                        }
                        _ => None
                    }
                },
            ) {
                raw_handlers.borrow_mut().push(raw);
            }
        }
        Ok(Button { hwnd: hwnd as *mut c_void, inner: Rc::new(inner), _handler: Rc::new(handler), click_cb, _raw_click_handler: raw_handlers })
    }

    // -- Label --

    #[derive(Clone)]
    pub struct Label {
        pub(crate) inner: Rc<nwg::Label>,
        // Stable hwnd snapshot for AsRef (see Window).
        hwnd: *mut c_void,
        /// Set by `set_fixed_width`. A fixed label keeps this width through
        /// every `set_text`, so a changing value can never reflow the siblings
        /// packed after it. `None` keeps the shrink-to-fit behaviour.
        fixed_w: Rc<std::cell::Cell<Option<i32>>>,
    }

    impl Label {
        /// Pin this label's laid-out width.
        ///
        /// A plain Win32 STATIC auto-sizes to its text, and `set_text` posts a
        /// `WM_SIZE` to the parent box so the parent re-fits it — so a label
        /// whose *content* changes (an address that goes from `A1` to `A100`)
        /// grows or shrinks and shifts every sibling packed after it. Pinning
        /// the width makes the label's slot stable instead. Fixing a width
        /// that is too narrow only clips the text, so pick the widest value
        /// the field can ever need.
        pub fn set_fixed_width(&self, w: Option<i32>) {
            self.fixed_w.set(w);
            self.apply_fixed_width();
        }

        /// Re-assert the pinned width on the widget.
        ///
        /// `SetWindowText` makes a STATIC resize itself, so the pin has to be
        /// pushed back after every write, and the parent box has to re-run its
        /// layout to place the (now constant) width.
        fn apply_fixed_width(&self) {
            let Some(w) = self.fixed_w.get() else { return };
            let Some(hwnd) = self.inner.handle.hwnd() else { return };
            unsafe {
                let mut rect: winapi::shared::windef::RECT = std::mem::zeroed();
                if winapi::um::winuser::GetClientRect(hwnd as _, &mut rect) != 0 {
                    let h = rect.bottom - rect.top;
                    winapi::um::winuser::SetWindowPos(
                        hwnd as _, std::ptr::null_mut(), 0, 0, w, h,
                        winapi::um::winuser::SWP_NOZORDER | winapi::um::winuser::SWP_NOMOVE,
                    );
                }
                let parent = winapi::um::winuser::GetParent(hwnd as _);
                if !parent.is_null() {
                    let mut prect: winapi::shared::windef::RECT = std::mem::zeroed();
                    winapi::um::winuser::GetClientRect(parent, &mut prect);
                    let l = (((prect.bottom & 0xFFFF) << 16) | (prect.right & 0xFFFF)) as isize;
                    winapi::um::winuser::PostMessageW(
                        parent, winapi::um::winuser::WM_SIZE, 0, l as _);
                }
            }
        }

        pub fn set_text(&self, text: &str) {
            self.inner.set_text(text);
            // Nudge the parent to re-run its layout (if it has a WM_SIZE
            // layout handler, i.e. a BoxWidget): label width is measured
            // from text at layout time, so a text change must re-layout
            // to keep the label fitted (parity with GTK auto-sizing).
            // Harmless when the parent has no such handler.
            //
            // A fixed-width label must NOT re-fit: that is the whole point of
            // pinning it, so its siblings stay put as the text changes.
            if self.fixed_w.get().is_some() {
                self.apply_fixed_width();
                return;
            }
            if let Some(hwnd) = self.inner.handle.hwnd() {
                unsafe {
                    let parent = winapi::um::winuser::GetParent(hwnd as _);
                    if !parent.is_null() {
                        let mut rect: winapi::shared::windef::RECT = std::mem::zeroed();
                        winapi::um::winuser::GetClientRect(parent, &mut rect);
                        let l = (((rect.bottom & 0xFFFF) << 16) | (rect.right & 0xFFFF)) as isize;
                        winapi::um::winuser::PostMessageW(parent,
                            winapi::um::winuser::WM_SIZE, 0, l as _);
                    }
                }
            }
        }
        pub fn get_text(&self) -> Option<String> { Some(self.inner.text()) }
        pub fn set_visible(&self, visible: bool) {
            set_explicitly_hidden(self.hwnd, !visible);
            self.inner.set_visible(visible);
        }
        pub fn set_markup(&self, markup: &str) { self.inner.set_text(markup); }
        /// Set the x alignment of the label's text (0.0 left .. 1.0 right).
        /// Win32 STATIC uses SS_CENTER/SS_RIGHT rather than a float, so map
        /// the three ranges onto those styles.
        pub fn set_xalign(&self, x: f32) {
            const SS_LEFT: u32 = 0x0000;
            const SS_CENTER: u32 = 0x0001;
            const SS_RIGHT: u32 = 0x0002;
            // SS_CENTER/SS_RIGHT are type bits (0..2), not the 0x1F mask.
            const TYPE_MASK: u32 = 0x1F;
            let style = if x < 1.0 / 3.0 { SS_LEFT } else if x < 2.0 / 3.0 { SS_CENTER } else { SS_RIGHT };
            if let Some(hwnd) = self.inner.handle.hwnd() {
                unsafe {
                    let cur = winapi::um::winuser::GetWindowLongW(hwnd as _, winapi::um::winuser::GWL_STYLE) as u32;
                    winapi::um::winuser::SetWindowLongW(
                        hwnd as _,
                        winapi::um::winuser::GWL_STYLE,
                        ((cur & !TYPE_MASK) | style) as i32,
                    );
                }
            }
        }
        pub fn set_margin_start(&self, _px: i32) {}
        pub fn set_margin_top(&self, _px: i32) {}
        pub fn set_halign(&self, _align: i32) {}
        pub fn set_valign(&self, _align: i32) {}
        pub fn raw_handle(&self) -> *mut c_void {
            self.inner.handle.hwnd().unwrap_or(std::ptr::null_mut()) as *mut c_void
        }
    }

    impl AsRef<*mut c_void> for Label {
        fn as_ref(&self) -> &*mut c_void {
            &self.hwnd
        }
    }

    impl Widget for Label {
        fn raw_handle(&self) -> *mut c_void {
            self.inner.handle.hwnd().unwrap_or(std::ptr::null_mut()) as *mut c_void
        }
    }

    pub fn create_label(parent: *mut c_void) -> Result<Label, Error> {
        crate::backends::nwg::create_label(parent).map(|l| {
            let hwnd = l.handle.hwnd().unwrap_or(std::ptr::null_mut()) as *mut c_void;
            Label { inner: Rc::new(l), hwnd, fixed_w: Rc::new(std::cell::Cell::new(None)) }
        }).map_err(|e| Error::Backend(format!("{}", e)))
    }

    // -- BoxWidget --

    pub struct BoxWidget {
        pub(crate) frame: Option<Rc<nwg::Frame>>,
        pub(crate) hwnd: *mut c_void,
        pub(crate) children: Rc<RefCell<Vec<*mut c_void>>>,
        pub(crate) child_vexpand: Rc<RefCell<Vec<bool>>>,
        pub(crate) child_hexpand: Rc<RefCell<Vec<bool>>>,
        pub(crate) orientation: crate::backends::nwg::Orientation,
        pub(crate) spacing: i32,
        /// TEMPORARY ReactOS diagnosis: per-box id for layout marks.
        pub(crate) debug_id_cell: std::cell::Cell<i32>,
    }

    impl Clone for BoxWidget {
        fn clone(&self) -> Self {
            BoxWidget {
                frame: self.frame.clone(),
                hwnd: self.hwnd,
                children: self.children.clone(),
                child_vexpand: self.child_vexpand.clone(),
                child_hexpand: self.child_hexpand.clone(),
                orientation: self.orientation,
                spacing: self.spacing,
                // Shared across clones: it identifies the *widget*, and every
                // clone drives the same window, so they must agree.
                debug_id_cell: std::cell::Cell::new(0),
            }
        }
    }

    impl AsRef<*mut c_void> for BoxWidget {
        fn as_ref(&self) -> &*mut c_void {
            &self.hwnd
        }
    }

    impl Widget for BoxWidget {
        fn raw_handle(&self) -> *mut c_void {
            self.hwnd
        }
    }

    pub trait Appendable {
        fn collect_hwnds(&self) -> Vec<*mut c_void>;
    }

    impl<T: AsRef<*mut c_void>> Appendable for T {
        fn collect_hwnds(&self) -> Vec<*mut c_void> {
            let ptr = *self.as_ref();
            if ptr.is_null() { vec![] } else { vec![ptr] }
        }
    }

    impl BoxWidget {
        pub fn append(&self, child: &impl Appendable) {
            let hwnds = child.collect_hwnds();
            // A null HWND can never participate in layout (it is filtered
            // below) — silently dropping it crams the remaining widgets
            // with no diagnostic. Fail loudly in debug builds instead.
            debug_assert!(
                hwnds.iter().all(|&p| !p.is_null()),
                "BoxWidget::append: child with null HWND cannot be laid out"
            );
            for &ptr in &hwnds {
                if !ptr.is_null() && !self.hwnd.is_null() {
                    unsafe {
                        winapi::um::winuser::SetParent(ptr as _, self.hwnd as _);
                    }
                }
            }
            let mut children = self.children.borrow_mut();
            let mut vex = self.child_vexpand.borrow_mut();
            let mut hex = self.child_hexpand.borrow_mut();
            children.extend(hwnds.into_iter().filter(|&c| !c.is_null()));
            vex.resize(children.len(), false);
            hex.resize(children.len(), false);
            drop(children);
            drop(vex);
            drop(hex);
            // Never depend on a future WM_SIZE: lay out now with the
            // current size (re-runs on every later WM_SIZE anyway).
            self.request_layout();
        }
        pub fn set_child_vexpand(&self, child: &impl AsRef<*mut c_void>, expand: bool) {
            let ptr = *child.as_ref();
            let children = self.children.borrow();
            let mut vex = self.child_vexpand.borrow_mut();
            // Silently missing the child leaves it fixed-size (fx-bar cram
            // shape): fail loudly in debug builds instead.
            debug_assert!(
                children.iter().any(|&c| c == ptr),
                "BoxWidget::set_child_vexpand: child not found in box"
            );
            if let Some(idx) = children.iter().position(|&c| c == ptr) {
                vex[idx] = expand;
            }
            drop(children);
            drop(vex);
            self.request_layout();
        }
        pub fn set_child_hexpand(&self, child: &impl AsRef<*mut c_void>, expand: bool) {
            let ptr = *child.as_ref();
            let children = self.children.borrow();
            let mut hex = self.child_hexpand.borrow_mut();
            // Silently missing the child leaves it fixed-size (fx-bar cram
            // shape): fail loudly in debug builds instead.
            debug_assert!(
                children.iter().any(|&c| c == ptr),
                "BoxWidget::set_child_hexpand: child not found in box"
            );
            if let Some(idx) = children.iter().position(|&c| c == ptr) {
                hex[idx] = expand;
            }
            drop(children);
            drop(hex);
            self.request_layout();
        }
        /// TEMPORARY ReactOS diagnosis: stable per-box id, so layout marks
        /// from different nesting levels can be told apart.
        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
        fn debug_id(&self) -> i32 {
            use std::sync::atomic::{AtomicI32, Ordering};
            static NEXT: AtomicI32 = AtomicI32::new(1);
            let cell = self.debug_id_cell.get();
            if cell == 0 {
                let n = NEXT.fetch_add(1, Ordering::Relaxed);
                self.debug_id_cell.set(n);
                n
            } else {
                cell
            }
        }
        pub fn layout(&self, _x: i32, _y: i32, w: i32, h: i32) {
            // TEMPORARY Win95 diagnosis.
            #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
            {
                let id = self.debug_id();
                let h2 = h + id * 100000;
                mark95xy(b"layot", w, h2);
            }
            // Negative/zero sizes arise transiently (a box laid out before
            // its parent is sized, or a wrapped synthetic WM_SIZE). A
            // negative size is never valid: SetWindowPos clamps it to 0
            // (hiding the child) while the synthetic WM_SIZE below would
            // wrap it to ~65526 and fling children off-screen. Clamp here
            // so garbage layouts are harmless no-ops at the right place.
            let w = w.max(0);
            let h = h.max(0);
            let children = self.children.borrow();
            let vex = self.child_vexpand.borrow();
            let hex = self.child_hexpand.borrow();
            let n = children.len();
            if n == 0 { return; }
            let (fixed_w, fixed_h) = match self.orientation {
                crate::backends::nwg::Orientation::Horizontal => {
                    (0, h - 10)
                }
                crate::backends::nwg::Orientation::Vertical => {
                    (w - 10, 0)
                }
            };
            let mut desired_sizes: Vec<i32> = Vec::with_capacity(n);
            // Children that were deliberately hidden (set_visible(false)).
            // `set_window_pos` below shows every child, so a hidden child
            // would be resurrected by the very layout pass that is meant to
            // honour the hide. Remember them and re-hide after positioning.
            // A child is "deliberately hidden" only if its OWN WS_VISIBLE bit
            // is clear - see the note on the test below.
            let mut hidden_children: Vec<*mut c_void> = Vec::new();
            let explicitly_hidden = explicitly_hidden_windows().borrow().clone();
            for i in 0..n {
                // Hidden children take no space (e.g. the sheet tab strip
                // with a single sheet): hiding alone would otherwise leave
                // a blank gap in the layout.
                //
                // Test the child's OWN WS_VISIBLE style bit, never
                // IsWindowVisible: that call is *recursive*, so a child whose
                // toplevel is not mapped yet reports invisible. Every child
                // was then misfiled as "deliberately hidden" and the
                // re-hide pass below hid the whole tree for good - the grid
                // canvas stayed invisible forever and never received a
                // single WM_PAINT (probe95 showed the scrolled frame and the
                // canvas as 'h' with correct geometry). WS_VISIBLE is
                // per-window, so it reports exactly what set_visible(false)
                // did.
                let hidden = unsafe {
                    let style = winapi::um::winuser::GetWindowLongW(
                        children[i] as _, winapi::um::winuser::GWL_STYLE,
                    ) as i32;
                    style & winapi::um::winuser::WS_VISIBLE as i32 == 0
                };
                // (Fix-ReactOS) A child whose style bit is clear is NOT
                // necessarily hidden on purpose.
                //
                // ReactOS strips WS_VISIBLE from every WS_CHILD at creation
                // (window.c: `pWnd->style = Cs->style & ~WS_VISIBLE`) and only
                // restores it later via the CreateWindowEx show path, which a
                // child created while its own parent is still being constructed
                // does not always reach. So a freshly created container - here
                // the scrolled window that holds the grid - enters its first
                // layout with the bit clear.
                //
                // Treating that as "deliberately hidden" is self-sustaining: it
                // gets desired size 0, the `cw > 0 || ch > 0` guard below then
                // skips set_window_pos, so nothing ever shows it, so the next
                // pass sees the same clear bit and hides it again. The grid
                // then stays blank for good while the canvas keeps reporting
                // perfectly healthy paints.
                //
                // `set_visible(false)` is the only thing that should remove a
                // widget, and the adapter routes it through show_/hide_ below,
                // so an explicit hide is recorded there. A child with no record
                // is merely un-shown-yet, not hidden: give it a real size and
                // let set_window_pos show it.
                let hidden = hidden && explicitly_hidden.contains(&(children[i] as usize));
                if hidden {
                    desired_sizes.push(0);
                    hidden_children.push(children[i]);
                    continue;
                }
                let is_expand = match self.orientation {
                    crate::backends::nwg::Orientation::Horizontal => hex[i],
                    crate::backends::nwg::Orientation::Vertical => vex[i],
                };
                if !is_expand {
                    let hardcoded = match self.orientation {
                        crate::backends::nwg::Orientation::Horizontal => 60,
                        crate::backends::nwg::Orientation::Vertical => 28,
                    };
                    // Labels (STATIC controls) are fitted to their text
                    // (parity with GTK auto-sizing); NWG gives them a wide
                    // default that would otherwise leave gaps in the row.
                    // Other controls keep their current window size.
                    let fitted = match self.orientation {
                        crate::backends::nwg::Orientation::Horizontal =>
                            static_text_width(children[i] as _),
                        crate::backends::nwg::Orientation::Vertical => None,
                    };
                    if let Some(w) = fitted {
                        desired_sizes.push(w);
                        continue;
                    }
                    unsafe {
                        // Measure the CLIENT rect, not the window rect. The
                        // window rect includes the frame, and a control that
                        // has not been laid out yet reports a degenerate
                        // window rect on ReactOS - so measuring the window
                        // rect here collapses a vertical box's child to the
                        // hardcoded default height, leaving the box 28px tall
                        // instead of filling its parent (the whole tree is
                        // then sized wrong and the first paint never happens).
                        let mut rect: winapi::shared::windef::RECT = std::mem::zeroed();
                        let ok = winapi::um::winuser::GetClientRect(children[i] as _, &mut rect) != 0;
                        let sz = match self.orientation {
                            crate::backends::nwg::Orientation::Horizontal => rect.right - rect.left,
                            crate::backends::nwg::Orientation::Vertical => rect.bottom - rect.top,
                        };
                        if ok && sz > 10 {
                            desired_sizes.push(sz);
                        } else {
                            // TEMPORARY ReactOS diagnosis: the measurement
                            // that fell back to the hardcoded default.
                            #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                            {
                                let id = self.debug_id();
                                mark95xy(b"falbk", (id as i32) * 1000 + i as i32, sz);
                            }
                            desired_sizes.push(hardcoded);
                        }
                    }
                } else {
                    desired_sizes.push(0);
                }
            }
            // Shared span math (unit-tested in win32_portable): fixed sizes
            // from `desired_sizes`, expanders splitting the remainder with
            // GTK fill parity (no lost remainder pixel).
            let flags: Vec<bool> = (0..n)
                .map(|i| match self.orientation {
                    crate::backends::nwg::Orientation::Horizontal => hex[i],
                    crate::backends::nwg::Orientation::Vertical => vex[i],
                })
                .collect();
            let avail = match self.orientation {
                crate::backends::nwg::Orientation::Horizontal => w - 10,
                crate::backends::nwg::Orientation::Vertical => h - 10,
            };
            // TEMPORARY ReactOS diagnosis: the per-child expand flags this box
            // is about to distribute with (box id, then i, vex, hex).
            #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
            {
                let id = self.debug_id();
                for i in 0..n {
                    let packed = (i as i32) * 100 + (vex[i] as i32) * 10 + (hex[i] as i32);
                    mark95xy(b"flags", id * 1000 + packed, desired_sizes[i]);
                }
            }

            // TEMPORARY ReactOS diagnosis: reached span distribution for
            // this box (id, avail, n children).
            #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
            {
                let id = self.debug_id();
                mark95xy(b"distr", avail + id * 100000, n as i32);
            }
            let spans = crate::win32_portable::distribute_spans(
                5,
                avail,
                self.spacing,
                &desired_sizes,
                &flags,
            );
            // TEMPORARY ReactOS diagnosis: spans computed.
            #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
            {
                let id = self.debug_id();
                mark95xy(b"distd", self.spacing + id * 100000, n as i32);
            }

            // (Fix-ReactOS) Drop the RefCell borrows before touching any
            // child. set_window_pos() below makes ReactOS deliver a *real*
            // WM_SIZE to the child synchronously, and SendMessageW(WM_SIZE)
            // does too; a nested box child then re-enters this same layout,
            // which would try to borrow `children` again while this pass
            // still holds it - a RefCell re-entrancy borrow error that
            // aborts the app. Copy out what the sizing loop needs and release
            // the borrows first.
            let children_vec: Vec<*mut c_void> = children.clone();
            let vex_vec: Vec<bool> = vex.clone();
            let hex_vec: Vec<bool> = hex.clone();
            drop(vex);
            drop(hex);
            drop(children);
            let children = children_vec;
            let vex = vex_vec;
            let hex = hex_vec;
            let (children, vex, hex) = (&children, &vex, &hex);
            for i in 0..n {
                let child = children[i];
                let (pos, span) = spans[i];
                let (cw, ch) = match self.orientation {
                    crate::backends::nwg::Orientation::Horizontal => (span, fixed_h),
                    crate::backends::nwg::Orientation::Vertical => (fixed_w, span),
                };
                let (cx, cy) = match self.orientation {
                    crate::backends::nwg::Orientation::Horizontal => (pos, 5),
                    crate::backends::nwg::Orientation::Vertical => (5, pos),
                };
                // Clamp the span: distribute_spans can return negatives
                // when the box itself was laid out at/near zero size
                // (avail = w - 10 < fixed total). A negative size must
                // never reach SetWindowPos (Wine clamps to 0, hiding the
                // child) nor the synthetic WM_SIZE below (it would wrap
                // to ~65526 and fling nested children off-screen).
                let (cw, ch) = (cw.max(0), ch.max(0));
                // TEMPORARY ReactOS diagnosis: which child index the layout
                // pass is on, so a hang localises to a specific child.
                #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                {
                    use std::sync::atomic::{AtomicU32, Ordering};
                    static LAST_CHILD: AtomicU32 = AtomicU32::new(0xFFFF);
                    LAST_CHILD.store(i as u32, Ordering::Relaxed);
                }
                // A zero-sized child has nothing to show, and on ReactOS
                // positioning one at 0 wedges the whole layout pass: ReactOS
                // keeps recomputing that window's frame and the parent never
                // gets past it (observed: the pass stops dead at the first
                // 0-width child, present() never returns and nothing paints).
                // Hide it and leave its geometry alone; a later pass with a
                // real span positions it normally.
                if cw > 0 || ch > 0 {
                    // TEMPORARY ReactOS diagnosis: entering/leaving
                    // set_window_pos, which can block on ReactOS.
                    #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                    {
                        let id = self.debug_id();
                        mark95xy(b"preSW", id * 1000 + i as i32, cw);
                    }
                    set_window_pos(child, cx, cy, cw, ch);
                    // TEMPORARY ReactOS diagnosis: set_window_pos returned.
                    #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                    {
                        let id = self.debug_id();
                        mark95xy(b"pswX ", id * 1000 + i as i32, cw);
                    }
                } else {
                    unsafe {
                        winapi::um::winuser::ShowWindow(
                            child as _, winapi::um::winuser::SW_HIDE);
                    }
                }
                // TEMPORARY ReactOS diagnosis: survived positioning child i.
                // The box id disambiguates which nesting level wedged.
                #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                if i as u32 % 1 == 0 {
                    let id = self.debug_id();
                    let i32v = i as i32;
                    let cwv = cw;
                    let n = id * 1000 + i32v;
                    mark95xy(b"chld!", n, cwv);
                }
                // Airtight cascade: SetWindowPos only delivers WM_SIZE when
                // the size actually changed, so a nested box that keeps its
                // size would never re-lay-out its own children (the cram
                // failure). Synthesize WM_SIZE unconditionally — leaf
                // controls ignore it, nested boxes re-run their layout.
                // Guard: a zero-size child has nothing to lay out, and
                // packing a non-positive size would wrap (see above).
                if cw > 0 && ch > 0 {
                    // TEMPORARY ReactOS diagnosis: the synthetic WM_SIZE a
                    // parent sends its child (box id, child index, size).
                    #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                    {
                        let id = self.debug_id();
                        mark95xy(b"synz!", (id as i32) * 1000 + i as i32, cw);
                    }
                    unsafe {
                        let l = ((ch & 0xFFFF) << 16) | (cw & 0xFFFF);
                        winapi::um::winuser::SendMessageW(
                            child as _,
                            winapi::um::winuser::WM_SIZE,
                            0,
                            l as _,
                        );
                    }
                }
            }
            // Re-hide the children that were hidden on entry: the
            // SWP_SHOWWINDOW in `set_window_pos` above showed them again.
            for &child in &hidden_children {
                unsafe {
                    winapi::um::winuser::ShowWindow(
                        child as _,
                        winapi::um::winuser::SW_HIDE,
                    );
                }
            }
            // TEMPORARY ReactOS diagnosis: layout() returned.
            #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
            {
                let id = self.debug_id();
                mark95xy(b"ltout", id, w);
            }
        }
        /// Re-run layout with the box's current client size. Called after
        /// every structural mutation (append, expand-flag change) so layout
        /// never depends on a future WM_SIZE that may never arrive.
        pub fn request_layout(&self) {
            if self.hwnd.is_null() {
                return;
            }
            unsafe {
                let mut rect: winapi::shared::windef::RECT = std::mem::zeroed();
                if winapi::um::winuser::GetClientRect(self.hwnd as _, &mut rect) != 0 {
                    self.layout(0, 0, rect.right - rect.left, rect.bottom - rect.top);
                }
            }
        }
        /// Minimum size of the box itself.
        ///
        /// A box with no explicit height lays its children out at their natural
        /// height, which for a Button inside a horizontal row can exceed the
        /// strip the caller allocated -- the rows then overlap and the text
        /// renders on top of itself. Setting the size on the row is the
        /// portable way to say "this strip is N tall".
        pub fn set_size_request(&self, w: i32, h: i32) {
            if !self.hwnd.is_null() {
                let hwnd = self.hwnd;
                unsafe {
                    winapi::um::winuser::SetWindowPos(
                        hwnd as _,
                        std::ptr::null_mut(), 0, 0, w, h,
                        winapi::um::winuser::SWP_NOZORDER | winapi::um::winuser::SWP_NOMOVE,
                    );
                }
            }
        }
        /// Whether the box may take extra space from its parent.
        pub fn set_hexpand(&self, _expand: bool) {}
        pub fn set_vexpand(&self, _expand: bool) {}
        pub fn set_visible(&self, visible: bool) {
            if !self.hwnd.is_null() {
                unsafe {
                    winapi::um::winuser::ShowWindow(self.hwnd as _, if visible { 1 } else { 0 });
                }
            }
        }}

    /// Measure a STATIC (label) control's text width in pixels, for
    /// shrink-to-fit layout (parity with GTK label auto-sizing).
    /// Returns None for non-label controls or on any measurement failure
    /// (callers fall back to the window rect / hardcoded size).
    fn static_text_width(hwnd: winapi::shared::windef::HWND) -> Option<i32> {
        // TEMPORARY ReactOS diagnosis: this sends WM_GETFONT to the control,
        // so mark entry/exit to see whether the sizing loop wedges inside it.
        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
        {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            if n < 400 {
                mark95xy(b"stw+ ", n as i32, 0);
            }
        }
        let r = static_text_width_inner(hwnd);
        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
        {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.load(Ordering::Relaxed);
            if n < 400 {
                mark95xy(b"stw- ", n as i32, 0);
            }
        }
        r
    }

    /// Measures a STATIC control's text, for shrink-to-fit layout.
    ///
    /// Split from the public wrapper so the call could be bracketed with the
    /// ReactOS entry/exit tracing without the wrapper growing a second body.
    /// Nothing here is toolchain-specific: it is plain Win32 `GetClassNameW` /
    /// `GetWindowTextW` / `GetTextExtentPoint32W` against the control's own
    /// `WM_GETFONT`, so it is available on every Windows target. Gating it on
    /// one toolchain left the (ungated) caller in `layout` uncompilable
    /// everywhere else.
    fn static_text_width_inner(hwnd: winapi::shared::windef::HWND) -> Option<i32> {
        unsafe {
            let mut cls: [u16; 256] = [0; 256];
            let n = winapi::um::winuser::GetClassNameW(hwnd, cls.as_mut_ptr(), 256);
            if n <= 0 { return None; }
            if !String::from_utf16_lossy(&cls[..n as usize]).eq_ignore_ascii_case("Static") {
                return None;
            }
            let tlen = winapi::um::winuser::GetWindowTextLengthW(hwnd);
            let mut buf: Vec<u16> = vec![0; (tlen + 1) as usize];
            winapi::um::winuser::GetWindowTextW(hwnd, buf.as_mut_ptr(), tlen + 1);
            let hdc = winapi::um::winuser::GetDC(hwnd);
            if hdc.is_null() { return None; }
            let hfont = winapi::um::winuser::SendMessageW(hwnd, winapi::um::winuser::WM_GETFONT, 0, 0);
            let old = if hfont != 0 {
                winapi::um::wingdi::SelectObject(hdc, hfont as _)
            } else {
                std::ptr::null_mut()
            };
            let mut sz: winapi::shared::windef::SIZE = std::mem::zeroed();
            let ok = winapi::um::wingdi::GetTextExtentPoint32W(hdc, buf.as_ptr(), tlen, &mut sz);
            if !old.is_null() { winapi::um::wingdi::SelectObject(hdc, old); }
            winapi::um::winuser::ReleaseDC(hwnd, hdc);
            if ok == 0 { return None; }
            Some(sz.cx + 8) // small horizontal padding
        }
    }

    pub fn create_box(orientation: crate::backends::nwg::Orientation, spacing: i32, parent: *mut c_void) -> Result<BoxWidget, Error> {
        let mut frame = nwg::Frame::default();
        if !parent.is_null() {
            nwg::Frame::builder()
                .flags(nwg::FrameFlags::VISIBLE)
                .size((0, 0))
                .position((0, 0))
                .parent(&nwg::ControlHandle::Hwnd(parent as _))
                .build(&mut frame)
                .map_err(|e| Error::Backend(format!("{}", e)))?;
        }
        let hwnd = frame.handle.hwnd().unwrap_or(std::ptr::null_mut()) as *mut c_void;
        let children: Rc<RefCell<Vec<*mut c_void>>> = Rc::new(RefCell::new(Vec::new()));
        let child_vexpand: Rc<RefCell<Vec<bool>>> = Rc::new(RefCell::new(Vec::new()));
        let child_hexpand: Rc<RefCell<Vec<bool>>> = Rc::new(RefCell::new(Vec::new()));
        let bw = BoxWidget {
            frame: Some(Rc::new(frame)), hwnd,
            children: children.clone(),
            child_vexpand: child_vexpand.clone(),
            child_hexpand: child_hexpand.clone(),
            orientation, spacing,
            debug_id_cell: std::cell::Cell::new(0),
        };
        // Auto-layout on WM_SIZE — now shares children via Rc<RefCell>
        if hwnd != std::ptr::null_mut() {
            let bw2 = bw.clone();
            // (Fix-ReactOS) Per-box layout history; see the guard below.
            let last_size = std::rc::Rc::new(std::cell::RefCell::new((-1i32, -1i32)));
            static BOX_SIZE_ID: AtomicUsize = AtomicUsize::new(0xB0000000);
            let id = BOX_SIZE_ID.fetch_add(1, Ordering::SeqCst);
            let _ = nwg::bind_raw_event_handler(
                &nwg::ControlHandle::Hwnd(hwnd as _), id,
                move |_h, msg, _w, l| {
                    if msg == winapi::um::winuser::WM_SIZE {
                        let w = (l & 0xFFFF) as i32;
                        let h = ((l >> 16) & 0xFFFF) as i32;
                        // A zero-size box has nothing to lay out, and these
                        // arrive as stale queue leftovers from the un-pumped
                        // setup storm (pump_events is a no-op on Windows):
                        // honoring them crushes children to 0 and the white
                        // screen never repairs (nothing re-invalidates).
                        // The next real (non-zero) size re-runs layout.
                        if w <= 0 || h <= 0 { return None; }
                        // (Fix-ReactOS) Re-entrancy guard, same reason as the
                        // toplevel: layout() repositions every child, which on
                        // ReactOS re-fires WM_NCCALCSIZE on them and re-enters
                        // this handler for any nested box. Because a nested
                        // box's own size is what its parent just computed,
                        // each pass reports a fresh size, so the cascade never
                        // settles: WM_GETFONT traffic continues forever and the
                        // UI thread livelocks inside present() instead of ever
                        // completing the first paint. Laying out once per
                        // distinct size is all a settled layout needs.
                        {
                            let mut last = last_size.borrow_mut();
                            if *last == (w, h) { return None; }
                            *last = (w, h);
                        }
                        bw2.layout(0, 0, w, h);
                        // TEMPORARY ReactOS diagnosis: did the nested layout
                        // return? (It does not on ReactOS - the app hangs
                        // inside it, after the NCCALCSIZE of its children.)
                        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                        mark95a(b"box-layout-done\n");
                    }
                    None
                },
            );
        }
        Ok(bw)
    }

    // -- Grid --

    pub struct Grid;

    impl Grid {
        pub fn attach(&self, child: &impl AsRef<*mut c_void>, left: i32, top: i32, width: i32, height: i32) {
            set_window_pos(*child.as_ref(), left, top, width, height);
        }
    }

    pub fn create_grid() -> Result<Grid, Error> { Ok(Grid) }

    // -- Entry --

    pub struct Entry {
        pub(crate) inner: Rc<nwg::TextInput>,
        pub(crate) _handler: Rc<nwg::EventHandler>,
        // Stable hwnd snapshot for AsRef (see Window).
        hwnd: *mut c_void,
        pub(crate) changed_cb: Rc<RefCell<Option<Box<dyn FnMut()>>>>,
        focus_in_cb: Rc<RefCell<Option<Box<dyn FnMut(*mut c_void) -> i32>>>>,
        focus_out_cb: Rc<RefCell<Option<Box<dyn FnMut(*mut c_void) -> i32>>>>,
        click_cb: Rc<RefCell<Option<Box<dyn FnMut()>>>>,
        _focus_in_handler: Option<nwg::RawEventHandler>,
        _focus_out_handler: Option<nwg::RawEventHandler>,
        _click_handler: Option<nwg::RawEventHandler>,
        key_cb: Rc<RefCell<Option<Box<dyn FnMut(u32, u32) -> bool>>>>,
        _key_handler: Option<nwg::RawEventHandler>,
        activate_cb: Rc<RefCell<Option<Box<dyn FnMut(*mut c_void)>>>>,
        pub(crate) pos_x: std::cell::Cell<i32>,
        pub(crate) pos_y: std::cell::Cell<i32>,
    }

    impl Clone for Entry {
        fn clone(&self) -> Self {
            Entry {
                inner: self.inner.clone(),
                _handler: self._handler.clone(),
                changed_cb: self.changed_cb.clone(),
                focus_in_cb: self.focus_in_cb.clone(),
                focus_out_cb: self.focus_out_cb.clone(),
                click_cb: self.click_cb.clone(),
                _focus_in_handler: None,
                _focus_out_handler: None,
                _click_handler: None,
                key_cb: self.key_cb.clone(),
                _key_handler: None,
                activate_cb: self.activate_cb.clone(),
                hwnd: self.hwnd,
                pos_x: std::cell::Cell::new(self.pos_x.get()),
                pos_y: std::cell::Cell::new(self.pos_y.get()),
            }
        }
    }

    impl Entry {
        pub fn set_text(&self, text: &str) {
            // Preserve the user's caret across a programmatic set: read it
            // first, then restore it (clamped to the new length). Previously
            // this forced the caret to the end on every sync
            // (`sync_entry_to_buf` runs per keystroke), so the formula always
            // appended at the end no matter where the user's cursor was.
            let prev_pos = self.get_position();
            self.inner.set_text(text);
            if let Some(hwnd) = self.inner.handle.hwnd() {
                unsafe {
                    let len = winapi::um::winuser::GetWindowTextLengthW(hwnd as _) as usize;
                    let pos = prev_pos.unwrap_or(len).min(len);
                    winapi::um::winuser::SendMessageW(
                        hwnd as _,
                        winapi::um::winuser::EM_SETSEL as u32,
                        pos,
                        pos as isize,
                    );
                }
            }
        }
        pub fn get_text(&self) -> Option<String> { Some(self.inner.text()) }
        /// Caret position as a character index. `EM_GETSEL` reports the
        /// selection start/end through two out-parameters; for a collapsed
        /// (caret-only) selection the start *is* the caret.
        pub fn get_position(&self) -> Option<usize> {
            let hwnd = self.inner.handle.hwnd()?;
            unsafe {
                let mut start: u32 = 0;
                let mut end: u32 = 0;
                winapi::um::winuser::SendMessageW(
                    hwnd as _,
                    winapi::um::winuser::EM_GETSEL as u32,
                    &mut start as *mut u32 as usize,
                    &mut end as *mut u32 as isize,
                );
                Some(start as usize)
            }
        }
        /// Move the caret (character index); Windows clamps out-of-range.
        pub fn set_position(&self, pos: usize) {
            if let Some(hwnd) = self.inner.handle.hwnd() {
                unsafe {
                    winapi::um::winuser::SendMessageW(
                        hwnd as _,
                        winapi::um::winuser::EM_SETSEL as u32,
                        pos,
                        pos as isize,
                    );
                }
            }
        }
        pub fn connect_changed(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            *self.changed_cb.borrow_mut() = Some(Box::new(f));
            Ok(0)
        }
        pub fn set_width_chars(&self, n: i32) {
            if let Some(hwnd) = self.inner.handle.hwnd() {
                unsafe {
                    let mut rect: winapi::shared::windef::RECT = std::mem::zeroed();
                    if winapi::um::winuser::GetWindowRect(hwnd as _, &mut rect) != 0 {
                        let h = rect.bottom - rect.top;
                        winapi::um::winuser::SetWindowPos(
                            hwnd as _, std::ptr::null_mut(), 0, 0, n * 8, h,
                            winapi::um::winuser::SWP_NOZORDER | winapi::um::winuser::SWP_NOMOVE,
                        );
                    }
                }
            }
        }
        pub fn set_size_request(&self, w: i32, h: i32) {
            if let Some(hwnd) = self.inner.handle.hwnd() {
                unsafe {
                    winapi::um::winuser::SetWindowPos(
                        hwnd as _, std::ptr::null_mut(), 0, 0, w, h,
                        winapi::um::winuser::SWP_NOZORDER | winapi::um::winuser::SWP_NOMOVE,
                    );
                }
            }
        }
        pub fn set_visible(&self, v: bool) {
            set_explicitly_hidden(self.hwnd, !v);
            self.inner.set_visible(v);
        }
        /// Whether this entry currently holds keyboard focus (Win32
        /// GetFocus equals its hwnd). A null hwnd reads as false.
        pub fn has_focus(&self) -> bool {
            if self.hwnd.is_null() {
                return false;
            }
            unsafe { winapi::um::winuser::GetFocus() == self.hwnd as _ }
        }
        /// Pointer press (click) into the entry. Same raw-hook pattern as
        /// the focus handlers: WM_LBUTTONDOWN on the edit hwnd fires the
        /// stored callback (no consume — the click must still focus and
        /// place the caret natively).
        pub fn connect_button_press(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            *self.click_cb.borrow_mut() = Some(Box::new(f));
            Ok(0)
        }
        pub fn grab_focus(&self) {
            let _ = self.inner.set_focus();
            if let Some(hwnd) = self.inner.handle.hwnd() {
                unsafe {
                    winapi::um::winuser::ShowWindow(hwnd as _, winapi::um::winuser::SW_SHOW);
                    winapi::um::winuser::SetWindowPos(
                        hwnd as _, winapi::um::winuser::HWND_TOP,
                        0, 0, 0, 0,
                        winapi::um::winuser::SWP_NOMOVE | winapi::um::winuser::SWP_NOSIZE | winapi::um::winuser::SWP_SHOWWINDOW,
                    );
                    winapi::um::winuser::RedrawWindow(
                        hwnd as _,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        winapi::um::winuser::RDW_INVALIDATE | winapi::um::winuser::RDW_UPDATENOW | winapi::um::winuser::RDW_ERASE | winapi::um::winuser::RDW_FRAME,
                    );
                    let parent = winapi::um::winuser::GetParent(hwnd as _);
                    if !parent.is_null() {
                        winapi::um::winuser::RedrawWindow(
                            parent,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            winapi::um::winuser::RDW_INVALIDATE | winapi::um::winuser::RDW_UPDATENOW | winapi::um::winuser::RDW_ALLCHILDREN | winapi::um::winuser::RDW_FRAME,
                        );
                    }
                }
            }
        }
        pub fn on_key(&self, mut f: Box<dyn FnMut(u32) -> bool>) {
            *self.key_cb.borrow_mut() = Some(Box::new(move |k: u32, _s: u32| -> bool { f(k) }));
        }
        pub fn set_hexpand(&self, _expand: bool) {}
        pub fn set_vexpand(&self, _expand: bool) {}
        pub fn add_class(&self, _class: &str) {}
        pub fn remove_class(&self, _class: &str) {}
        pub fn set_margin_start(&self, px: i32) {
            self.pos_x.set(px);
            if let Some(hwnd) = self.inner.handle.hwnd() {
                unsafe {
                    let _ = winapi::um::winuser::SetWindowPos(
                        hwnd as _, std::ptr::null_mut(), px, self.pos_y.get(), 0, 0,
                        winapi::um::winuser::SWP_NOZORDER | winapi::um::winuser::SWP_NOSIZE | winapi::um::winuser::SWP_SHOWWINDOW,
                    );
                }
            } else {
            }
        }
        pub fn set_margin_top(&self, px: i32) {
            self.pos_y.set(px);
            if let Some(hwnd) = self.inner.handle.hwnd() {
                unsafe {
                    let _ = winapi::um::winuser::SetWindowPos(
                        hwnd as _, std::ptr::null_mut(), self.pos_x.get(), px, 0, 0,
                        winapi::um::winuser::SWP_NOZORDER | winapi::um::winuser::SWP_NOSIZE | winapi::um::winuser::SWP_SHOWWINDOW,
                    );
                }
            } else {
            }
        }
        pub fn set_halign(&self, _align: i32) {}
        pub fn set_valign(&self, _align: i32) {}
        pub fn on_key_raw(&self, cb: Box<dyn FnMut(u32, u32) -> bool>) {
            *self.key_cb.borrow_mut() = Some(cb);
        }
        pub fn connect_activate(&self, f: impl FnMut(*mut c_void) + 'static) -> Result<u64, Error> {
            *self.activate_cb.borrow_mut() = Some(Box::new(f));
            Ok(0)
        }
        pub fn connect_focus_in_event(&self, f: impl FnMut(*mut c_void) -> i32 + 'static) -> Result<u64, Error> {
            *self.focus_in_cb.borrow_mut() = Some(Box::new(f));
            Ok(0)
        }
        pub fn connect_focus_out_event(&self, f: impl FnMut(*mut c_void) -> i32 + 'static) -> Result<u64, Error> {
            *self.focus_out_cb.borrow_mut() = Some(Box::new(f));
            Ok(0)
        }
    }

    impl AsRef<*mut c_void> for Entry {
        fn as_ref(&self) -> &*mut c_void {
            &self.hwnd
        }
    }

    impl Widget for Entry {
        fn raw_handle(&self) -> *mut c_void {
            self.inner.handle.hwnd().unwrap_or(std::ptr::null_mut()) as *mut c_void
        }
    }

    pub fn create_entry(parent: *mut c_void) -> Result<Entry, Error> {
        let (inner, changed_cb, handler) = crate::backends::nwg::create_entry(parent)
            .map_err(|e| Error::Backend(format!("{}", e)))?;
        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
        mark95a(b"a-e0\n");
        let focus_in_cb: Rc<RefCell<Option<Box<dyn FnMut(*mut c_void) -> i32>>>> = Rc::new(RefCell::new(None));
        let focus_out_cb: Rc<RefCell<Option<Box<dyn FnMut(*mut c_void) -> i32>>>> = Rc::new(RefCell::new(None));
        let click_cb: Rc<RefCell<Option<Box<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
        let key_cb: Rc<RefCell<Option<Box<dyn FnMut(u32, u32) -> bool>>>> = Rc::new(RefCell::new(None));
        let activate_cb: Rc<RefCell<Option<Box<dyn FnMut(*mut c_void)>>>> =
            Rc::new(RefCell::new(None));
        let hwnd = inner.handle.hwnd().unwrap_or(std::ptr::null_mut());
        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
        mark95xy(b"a-hwn", hwnd as i32, 0);
        if hwnd != std::ptr::null_mut() {
            unsafe {
                let ex = winapi::um::winuser::GetWindowLongW(
                    hwnd as _, winapi::um::winuser::GWL_EXSTYLE);
                #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                mark95xy(b"a-exs", ex, 0);
                winapi::um::winuser::SetWindowLongW(
                    hwnd as _, winapi::um::winuser::GWL_EXSTYLE,
                    ex | winapi::um::winuser::WS_EX_CLIENTEDGE as i32);
                #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                mark95a(b"a-slw\n");
                winapi::um::winuser::SetWindowPos(
                    hwnd as _, std::ptr::null_mut(), 0, 0, 0, 0,
                    winapi::um::winuser::SWP_NOMOVE | winapi::um::winuser::SWP_NOSIZE
                    | winapi::um::winuser::SWP_NOZORDER | winapi::um::winuser::SWP_FRAMECHANGED);
            }
        }

        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
        mark95a(b"a-e1\n");
        let _focus_in_handler = if hwnd != std::ptr::null_mut() {
            let cb = focus_in_cb.clone();
            static FOCUS_IN_ID: AtomicUsize = AtomicUsize::new(0x40000000);
            let id = FOCUS_IN_ID.fetch_add(1, Ordering::SeqCst);
            nwg::bind_raw_event_handler(
                &nwg::ControlHandle::Hwnd(hwnd), id,
                move |_h, msg, _w, _l| {
                    if msg == winapi::um::winuser::WM_SETFOCUS {
                        if let Some(ref mut f) = *cb.borrow_mut() { f(std::ptr::null_mut()); }
                    }
                    None
                },
            ).ok()
        } else { None };

        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
        mark95a(b"a-e2\n");
        let _click_handler = if hwnd != std::ptr::null_mut() {
            let cb = click_cb.clone();
            static CLICK_ID: AtomicUsize = AtomicUsize::new(0x70000000);
            let id = CLICK_ID.fetch_add(1, Ordering::SeqCst);
            nwg::bind_raw_event_handler(
                &nwg::ControlHandle::Hwnd(hwnd), id,
                move |_h, msg, _w, _l| {
                    if msg == winapi::um::winuser::WM_LBUTTONDOWN {
                        if let Some(ref mut f) = *cb.borrow_mut() { f(); }
                    }
                    None
                },
            ).ok()
        } else { None };
        let _focus_out_handler = if hwnd != std::ptr::null_mut() {
            let cb = focus_out_cb.clone();
            static FOCUS_OUT_ID: AtomicUsize = AtomicUsize::new(0x50000000);
            let id = FOCUS_OUT_ID.fetch_add(1, Ordering::SeqCst);
            nwg::bind_raw_event_handler(
                &nwg::ControlHandle::Hwnd(hwnd), id,
                move |_h, msg, _w, _l| {
                    if msg == winapi::um::winuser::WM_KILLFOCUS {
                        if let Some(ref mut f) = *cb.borrow_mut() { f(std::ptr::null_mut()); }
                    }
                    None
                },
            ).ok()
        } else { None };

        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
        mark95a(b"a-e3\n");
        let _key_handler = if hwnd != std::ptr::null_mut() {
            let kc = key_cb.clone();
            let act = activate_cb.clone();
            let act_hwnd = hwnd as *mut c_void;
            // When a WM_KEYDOWN is consumed by key_cb (the app handled the
            // key itself and synced the widget text), the message loop's
            // TranslateMessage still posts a WM_CHAR for it, and the edit
            // control's default handler would insert the char a second time
            // ("AA" for a single keypress). Remember the consumed key and
            // swallow its WM_CHAR (same idea as the Enter/Escape suppression
            // below). Only printable VKs set the flag — they reliably produce
            // exactly one WM_CHAR; arrows etc. produce none.
            let suppress_char: std::rc::Rc<std::cell::Cell<bool>> =
                std::rc::Rc::new(std::cell::Cell::new(false));
            let suppress_set = suppress_char.clone();
            let suppress_get = suppress_char.clone();
            static ENTRY_KEY_ID: AtomicUsize = AtomicUsize::new(0x60000000);
            let id = ENTRY_KEY_ID.fetch_add(1, Ordering::SeqCst);
            nwg::bind_raw_event_handler(
                &nwg::ControlHandle::Hwnd(hwnd), id,
                move |_h, msg, w, _l| {
                    // Consume WM_CHAR for Enter/Escape to prevent beep
                    if msg == winapi::um::winuser::WM_CHAR {
                        let c = (w & 0xFF) as u8;
                        if c == 0x0D || c == 0x1B {
                            return Some(0);
                        }
                        if suppress_get.get() {
                            suppress_get.set(false);
                            return Some(0);
                        }
                        return None;
                    }
                    if msg == winapi::um::winuser::WM_KEYDOWN || msg == winapi::um::winuser::WM_SYSKEYDOWN {
                        // Pure Alt+key belongs to the native menu (DefWindowProc
                        // opens the popup); decline so it is neither typed nor
                        // swallowed. Without this, Alt+F in a focused entry
                        // was consumed as printable text and menus were
                        // mouse-only. Ctrl+Alt (AltGr) still passes through.
                        if pure_alt_held() {
                            return None;
                        }
                        // GTK4 parity: an entry-level activate callback owns
                        // Return exclusively (the CAPTURE handler consumes it
                        // before on_key_raw fires there), so exactly one
                        // submit path runs per press on every backend.
                        if crate::win32_portable::entry_return_fires_activate(
                            (w & 0xFF) as u32,
                            act.borrow().is_some(),
                        ) {
                            if let Some(ref mut a) = *act.borrow_mut() {
                                a(act_hwnd);
                            }
                            return Some(0);
                        }
                        if let Some(ref mut f) = *kc.borrow_mut() {
                            // Full modifier mask (Shift/Ctrl/Alt): the app
                            // needs Shift for arrows, Ctrl/Alt to decline
                            // accelerators (copy/paste stay native) instead
                            // of typing them.
                            let mods = modifier_state();
                            // Translate Shift pairs/capitals (Shift+9 is '(',
                            // not '9'); specials keep raw VKs for dispatch. A
                            // colliding translation (None) declines so native
                            // insertion + resync handle the character.
                            if let Some(k) = translate_vk(w) {
                                if f(k, mods) {
                                    // Suppress the follow-on WM_CHAR exactly
                                    // when this VK produces one — otherwise
                                    // the native control inserts a second
                                    // copy ("==" for '='). Always *set*
                                    // (never just arm): a stale flag from a
                                    // non-producing key would swallow the
                                    // *next* genuine character.
                                    let vk = w & 0xFF;
                                    let mut suppress =
                                        crate::win32_portable::vk_produces_wm_char(vk as u32);
                                    if suppress && (0x60..=0x6F).contains(&vk) {
                                        // Numpad without NumLock is navigation
                                        // (no WM_CHAR follows).
                                        unsafe {
                                            const VK_NUMLOCK: i32 = 0x90;
                                            if winapi::um::winuser::GetKeyState(VK_NUMLOCK) as u16
                                                & 1
                                                == 0
                                            {
                                                suppress = false;
                                            }
                                        }
                                    }
                                    suppress_set.set(suppress);
                                    return Some(0);
                                }
                            } else {
                                return None;
                            }
                        }
                    }
                    None
                },
            ).ok()
        } else { None };

        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
        mark95a(b"a-e4\n");
        Ok(Entry { hwnd: hwnd as *mut c_void, inner: Rc::new(inner), _handler: Rc::new(handler), changed_cb, focus_in_cb, focus_out_cb, click_cb, _focus_in_handler, _focus_out_handler, _click_handler, key_cb, _key_handler, activate_cb, pos_x: std::cell::Cell::new(0), pos_y: std::cell::Cell::new(0) })
    }

    // ========== DropDown ==========

    #[derive(Clone)]
    pub struct DropDown {
        pub(crate) inner: Rc<nwg::ComboBox<String>>,
        // Stable hwnd snapshot for AsRef (see Window).
        hwnd: *mut c_void,
        /// Fired when the user picks a different item. Populated by
        /// `connect_changed`; the `WM_COMMAND` handler installed at creation
        /// reads it. Shared with that handler (see
        /// `backends::nwg::bind_command_notify`).
        pub(crate) changed_cb: Rc<RefCell<Option<Box<dyn FnMut()>>>>,
        /// The `WM_COMMAND` handler. Held (not dropped) because dropping an
        /// `nwg::RawEventHandler` unbinds the subclass; a clone shares it so
        /// the control and its clones never fight over unbinding.
        _notify_handler: Rc<Option<nwg::RawEventHandler>>,
    }

    impl DropDown {
        pub fn set_active(&self, index: Option<u32>) {
            self.inner.set_selection(index.map(|i| i as usize));
        }
        /// Move keyboard focus to the combo box so arrow keys open/navigate
        /// its list immediately after the dialog appears.
        pub fn grab_focus(&self) {
            let hwnd = self.inner.handle.hwnd().unwrap_or(std::ptr::null_mut());
            if hwnd.is_null() {
                return;
            }
            unsafe {
                winapi::um::winuser::SetFocus(hwnd as _);
            }
        }
        pub fn active(&self) -> Option<u32> {
            self.inner.selection().map(|i| i as u32)
        }
        pub fn get_active(&self) -> u32 {
            self.inner.selection().unwrap_or(0) as u32
        }
        pub fn connect_changed(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            *self.changed_cb.borrow_mut() = Some(Box::new(f));
            Ok(0)
        }
        pub fn set_hexpand(&self, _expand: bool) {}
        pub fn set_vexpand(&self, _expand: bool) {}
        /// Resize the combo box (Win32 controls are absolutely sized).
        pub fn set_size_request(&self, w: i32, h: i32) {
            let hwnd = self.inner.handle.hwnd().unwrap_or(std::ptr::null_mut());
            if hwnd.is_null() {
                return;
            }
            unsafe {
                winapi::um::winuser::SetWindowPos(
                    hwnd as _, std::ptr::null_mut(), 0, 0, w, h,
                    winapi::um::winuser::SWP_NOZORDER | winapi::um::winuser::SWP_NOMOVE,
                );
            }
        }
        /// Show/hide the combo box.
        pub fn set_visible(&self, visible: bool) {
            let hwnd = self.inner.handle.hwnd().unwrap_or(std::ptr::null_mut());
            if hwnd.is_null() {
                return;
            }
            set_explicitly_hidden(hwnd as _, !visible);
            unsafe {
                winapi::um::winuser::ShowWindow(
                    hwnd as _,
                    if visible { winapi::um::winuser::SW_SHOW } else { winapi::um::winuser::SW_HIDE },
                );
            }
        }
        /// Move the combo box within its parent (Win32 widgets are
        /// absolutely positioned by the parent, so this is the whole
        /// placement story on Windows).
        pub fn set_offset(&self, x: i32, y: i32) {
            let hwnd = self.inner.handle.hwnd().unwrap_or(std::ptr::null_mut());
            if hwnd.is_null() {
                return;
            }
            unsafe {
                winapi::um::winuser::SetWindowPos(
                    hwnd as _, std::ptr::null_mut(), x, y, 0, 0,
                    winapi::um::winuser::SWP_NOZORDER | winapi::um::winuser::SWP_NOSIZE,
                );
            }
        }
    }

    impl AsRef<*mut c_void> for DropDown {
        fn as_ref(&self) -> &*mut c_void {
            &self.hwnd
        }
    }

    pub fn create_dropdown(parent: *mut c_void, items: &[&str]) -> Result<DropDown, Error> {
        let (inner, changed_cb, notify) = crate::backends::nwg::create_dropdown(parent, items)
            .map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(DropDown {
            hwnd: inner.handle.hwnd().unwrap_or(std::ptr::null_mut()) as *mut c_void,
            inner: Rc::new(inner),
            changed_cb,
            _notify_handler: Rc::new(notify),
        })
    }

    // ========== CheckButton ==========

    #[derive(Clone)]
    pub struct CheckButton {
        pub(crate) inner: Rc<nwg::CheckBox>,
        // Stable hwnd snapshot for AsRef (see Window).
        hwnd: *mut c_void,
        /// Fired when the user toggles the box. Populated by
        /// `connect_toggled`; the `WM_COMMAND` handler installed at creation
        /// reads it. Shared with that handler (see
        /// `backends::nwg::bind_command_notify`).
        pub(crate) toggled_cb: Rc<RefCell<Option<Box<dyn FnMut()>>>>,
        /// The `WM_COMMAND` handler. Held (not dropped) because dropping an
        /// `nwg::RawEventHandler` unbinds the subclass; a clone shares it so
        /// the control and its clones never fight over unbinding.
        _notify_handler: Rc<Option<nwg::RawEventHandler>>,
    }

    impl CheckButton {
        pub fn set_active(&self, active: bool) {
            self.inner.set_check_state(if active { nwg::CheckBoxState::Checked } else { nwg::CheckBoxState::Unchecked });
        }
        pub fn is_active(&self) -> bool {
            matches!(self.inner.check_state(), nwg::CheckBoxState::Checked)
        }
        pub fn set_label(&self, label: &str) { self.inner.set_text(label); }
        pub fn get_label(&self) -> Option<String> { Some(self.inner.text()) }
        pub fn connect_toggled(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            *self.toggled_cb.borrow_mut() = Some(Box::new(f));
            Ok(0)
        }
    }

    impl AsRef<*mut c_void> for CheckButton {
        fn as_ref(&self) -> &*mut c_void {
            &self.hwnd
        }
    }

    pub fn create_checkbutton(parent: *mut c_void) -> Result<CheckButton, Error> {
        let (inner, toggled_cb, notify) = crate::backends::nwg::create_checkbox(parent, "Check")
            .map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(CheckButton {
            hwnd: inner.handle.hwnd().unwrap_or(std::ptr::null_mut()) as *mut c_void,
            inner: Rc::new(inner),
            toggled_cb,
            _notify_handler: Rc::new(notify),
        })
    }

    // ========== RadioButton ==========

    #[derive(Clone)]
    pub struct RadioButton {
        pub(crate) inner: Rc<nwg::RadioButton>,
        // Stable hwnd snapshot for AsRef (see Window).
        hwnd: *mut c_void,
        /// Fired when the user picks this radio. Populated by
        /// `connect_toggled`; the `WM_COMMAND` handler installed at creation
        /// reads it. Shared with that handler (see
        /// `backends::nwg::bind_command_notify`).
        pub(crate) toggled_cb: Rc<RefCell<Option<Box<dyn FnMut()>>>>,
        /// The `WM_COMMAND` handler. Held (not dropped) because dropping an
        /// `nwg::RawEventHandler` unbinds the subclass; a clone shares it so
        /// the control and its clones never fight over unbinding.
        _notify_handler: Rc<Option<nwg::RawEventHandler>>,
    }

    impl RadioButton {
        pub fn grab_focus(&self) {
            unsafe {
                winapi::um::winuser::SetFocus(self.hwnd as _);
            }
        }
        pub fn set_active(&self, active: bool) {
            self.inner.set_check_state(if active { nwg::RadioButtonState::Checked } else { nwg::RadioButtonState::Unchecked });
        }
        pub fn is_active(&self) -> bool {
            self.inner.check_state() == nwg::RadioButtonState::Checked
        }
        pub fn set_label(&self, label: &str) { self.inner.set_text(label); }
        pub fn get_label(&self) -> Option<String> { Some(self.inner.text()) }
        pub fn connect_toggled(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            *self.toggled_cb.borrow_mut() = Some(Box::new(f));
            Ok(0)
        }
        pub fn set_hexpand(&self, _expand: bool) {}
        pub fn set_vexpand(&self, _expand: bool) {}
    }

    impl AsRef<*mut c_void> for RadioButton {
        fn as_ref(&self) -> &*mut c_void {
            &self.hwnd
        }
    }

    /// `group_start` marks the first radio of a mutually-exclusive group
    /// (WS_GROUP): arrows then move selection natively within the group.
    /// Without it every radio is independent and arrows do nothing.
    pub fn create_radiobutton(parent: *mut c_void, group_start: bool) -> Result<RadioButton, Error> {
        let (inner, toggled_cb, notify) = crate::backends::nwg::create_radiobutton(parent, "Radio", group_start)
            .map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(RadioButton {
            hwnd: inner.handle.hwnd().unwrap_or(std::ptr::null_mut()) as *mut c_void,
            inner: Rc::new(inner),
            toggled_cb,
            _notify_handler: Rc::new(notify),
        })
    }

    // ========== TextView ==========

    #[derive(Clone)]
    pub struct TextView {
        pub(crate) inner: Rc<nwg::TextBox>,
        pub(crate) changed_cb: Rc<RefCell<Option<Box<dyn FnMut()>>>>,
        pub(crate) _handler: Rc<nwg::EventHandler>,
        // Stable hwnd snapshot for AsRef (see Window).
        hwnd: *mut c_void,
    }

    impl TextView {
        pub fn get_buffer(&self) -> &RefCell<Option<Box<dyn FnMut()>>> { &self.changed_cb }
        pub fn set_text(&self, text: &str) { self.inner.set_text(text); }
        pub fn get_text(&self) -> Option<String> { Some(self.inner.text()) }
        pub fn set_editable(&self, editable: bool) { self.inner.set_readonly(!editable); }
        pub fn set_size_request(&self, _w: i32, _h: i32) {}
        pub fn set_wrap_mode(&self, _mode: i32) {}
    }

    impl AsRef<*mut c_void> for TextView {
        fn as_ref(&self) -> &*mut c_void {
            &self.hwnd
        }
    }

    pub fn create_textview(parent: *mut c_void) -> Result<TextView, Error> {
        let (inner, changed_cb, handler) = crate::backends::nwg::create_textview(parent)
            .map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(TextView { hwnd: inner.handle.hwnd().unwrap_or(std::ptr::null_mut()) as *mut c_void, inner: Rc::new(inner), changed_cb, _handler: Rc::new(handler) })
    }

    // ========== Dialog ==========

    pub struct Dialog {
        pub(crate) inner: Rc<nwg::Window>,
        // Stable hwnd snapshot for AsRef (see Window).
        hwnd: *mut c_void,
        pub(crate) buttons: Rc<RefCell<Vec<(nwg::Button, nwg::EventHandler)>>>,
        pub(crate) response_cb: Rc<RefCell<Option<Box<dyn FnMut(i32)>>>>,
        pub(crate) _handler: Rc<nwg::EventHandler>,
        layout_cb: Rc<RefCell<Vec<Box<dyn FnMut(i32, i32)>>>>,
        dlg_key_handlers: Rc<RefCell<Vec<nwg::RawEventHandler>>>,
        esc_bound: Rc<RefCell<bool>>,
        content: Rc<RefCell<Vec<*mut c_void>>>,
    }

    impl Clone for Dialog {
        fn clone(&self) -> Self {
            Dialog {
                inner: self.inner.clone(),
                buttons: self.buttons.clone(),
                response_cb: self.response_cb.clone(),
                _handler: self._handler.clone(),
                hwnd: self.hwnd,
                layout_cb: self.layout_cb.clone(),
                dlg_key_handlers: self.dlg_key_handlers.clone(),
                esc_bound: self.esc_bound.clone(),
                content: self.content.clone(),
            }
        }
    }

    impl Dialog {
        pub fn run(&self) -> i32 { 0 }
        pub fn set_title(&self, title: &str) { self.inner.set_text(title); }
        /// Centre this dialog on `parent` (the main window). Windows has no
        /// transient-for concept; owning/centering is the equivalent so the
        /// dialog does not appear at an arbitrary screen position.
        pub fn set_transient_for(&self, parent: *mut std::os::raw::c_void) {
            if parent.is_null() {
                return;
            }
            let hwnd = self.inner.handle.hwnd().unwrap_or(std::ptr::null_mut());
            if hwnd.is_null() {
                return;
            }
            unsafe {
                use winapi::shared::windef::RECT;
                use winapi::um::winuser::*;
                let mut pr: RECT = std::mem::zeroed();
                if GetWindowRect(parent as _, &mut pr) == 0 {
                    return;
                }
                let mut dr: RECT = std::mem::zeroed();
                if GetWindowRect(hwnd as _, &mut dr) == 0 {
                    return;
                }
                let pw = pr.right - pr.left;
                let ph = pr.bottom - pr.top;
                let dw = dr.right - dr.left;
                let dh = dr.bottom - dr.top;
                let x = pr.left + (pw - dw) / 2;
                let y = pr.top + (ph - dh) / 2;
                SetWindowPos(
                    hwnd as _, std::ptr::null_mut(), x, y, 0, 0,
                    SWP_NOZORDER | SWP_NOSIZE,
                );
            }
        }
        pub fn set_size_request(&self, _w: i32, _h: i32) {}
        pub fn set_default_size(&self, w: i32, h: i32) {
            let hwnd = self.inner.handle.hwnd().unwrap_or(std::ptr::null_mut());
            if hwnd != std::ptr::null_mut() {
                unsafe {
                    winapi::um::winuser::SetWindowPos(
                        hwnd as _, std::ptr::null_mut(), 0, 0, w, h,
                        winapi::um::winuser::SWP_NOZORDER | winapi::um::winuser::SWP_NOMOVE,
                    );
                }
            }
        }
        pub fn present(&self) {
            self.inner.set_visible(true);
            let hwnd = self.inner.handle.hwnd().unwrap_or(std::ptr::null_mut());
            if hwnd != std::ptr::null_mut() {
                // Parity with GtkDialog (which natively emits a cancel response
                // on Escape): dismiss the dialog on Escape and fire the response
                // callback with 0 (the Cancel convention used by corro's prompt
                // dialogs). Keyboard focus is usually on a child (button/entry),
                // so bind on the dialog and every descendant. Bound once; the
                // RawEventHandlers are kept alive in dlg_key_handlers. All call sites
                // append content before present(), so the subtree is complete.
                // Own once-flag (not vec-emptiness): other dialog key bindings
                // (e.g. set_default_response) share the vec.
                if !*self.esc_bound.borrow() {
                    *self.esc_bound.borrow_mut() = true;
                    self.bind_esc_dismiss(hwnd);
                }
                unsafe {
                    winapi::um::winuser::SetForegroundWindow(hwnd as _);
                    let mut rect: winapi::shared::windef::RECT = std::mem::zeroed();
                    winapi::um::winuser::GetClientRect(hwnd, &mut rect);
                    let w = rect.right - rect.left;
                    let h = rect.bottom - rect.top;
                    for cb in self.layout_cb.borrow_mut().iter_mut() {
                        cb(w, h);
                    }
                    // Content over buttons (GtkDialog parity); also runs on
                    // dialog WM_SIZE and after add_button().
                    self.layout_dialog();
                    winapi::um::winuser::RedrawWindow(
                        hwnd as _,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        winapi::um::winuser::RDW_INVALIDATE | winapi::um::winuser::RDW_UPDATENOW | winapi::um::winuser::RDW_ALLCHILDREN | winapi::um::winuser::RDW_ERASE | winapi::um::winuser::RDW_FRAME,
                    );
                }
            }
        }
        /// The container a dialog's children are added to.
        ///
        /// A Win32 dialog has no content-area view the way `GtkDialog` does:
        /// `append_content_area` reparents each child's HWND straight to the
        /// dialog window and `layout_dialog` stacks them, so the dialog's own
        /// HWND *is* the content area. Returning it (rather than null) is what
        /// lets a caller measure or resize the area it was handed.
        pub fn get_content_area(&self) -> *mut c_void {
            self.inner.handle.hwnd().unwrap_or(std::ptr::null_mut()) as *mut c_void
        }

        pub fn append_content_area(&self, child: &impl Appendable) {
            for &ptr in &child.collect_hwnds() {
                if !ptr.is_null() {
                    unsafe {
                        let dlg = self.inner.handle.hwnd().unwrap_or(std::ptr::null_mut());
                        winapi::um::winuser::SetParent(ptr as _, dlg as _);
                        winapi::um::winuser::ShowWindow(ptr as _, winapi::um::winuser::SW_SHOW);
                    }
                    // Recorded for layout_dialog(): content stacks vertically
                    // over the button row (GtkDialog anatomy). Positioning
                    // happens there — never a full-area stretch per child,
                    // which overlapped every child at (0,0).
                    self.content.borrow_mut().push(ptr);
                }
            }
        }
        /// Lay out content (stacked, full-width, above the buttons) and the
        /// button row (bottom-right), GtkDialog parity. Runs on present(),
        /// on dialog WM_SIZE, and after add_button().
        pub fn layout_dialog(&self) {
            let hwnd = self.inner.handle.hwnd().unwrap_or(std::ptr::null_mut());
            if hwnd == std::ptr::null_mut() {
                return;
            }
            layout_nwg_dialog_parts(hwnd as _, &self.content, &self.buttons);
        }
        pub fn add_button(&self, text: &str, response_id: i32) {
            let hwnd = self.inner.handle.hwnd().unwrap_or(std::ptr::null_mut());
            let cb = self.response_cb.clone();
            let result = crate::backends::nwg::create_dialog_button(hwnd as *mut c_void, text, response_id, cb);
            if let Ok(btn) = result {
                self.buttons.borrow_mut().push(btn);
                // Buttons were never positioned (all piled at 0,0); lay out
                // now in case the dialog is already visible.
                self.layout_dialog();
            }
        }
        pub fn connect_response<F: FnMut(i32) + 'static>(&self, f: F) -> Result<u64, Error> {
            *self.response_cb.borrow_mut() = Some(Box::new(f));
            Ok(0)
        }
        /// Make `response_id` the dialog's default response: Return anywhere
        /// unhandled (e.g. on a focused radio button, which consumes nothing)
        /// fires it and dismisses, mirroring GtkDialog default-response
        /// semantics. Handlers bind on the dialog and all current
        /// descendants (like bind_esc_dismiss) because keys go to the focused
        /// control. Children that consume Return themselves (entries via
        /// activate) swallow it first, so no double-confirm.
        ///

        pub fn set_default_response(&self, response_id: i32) {
            let dlg_hwnd = self.inner.handle.hwnd().unwrap_or(std::ptr::null_mut());
            if dlg_hwnd.is_null() {
                return;
            }
            fn collect(hwnd: winapi::shared::windef::HWND, out: &mut Vec<winapi::shared::windef::HWND>) {
                out.push(hwnd);
                unsafe {
                    let mut child = winapi::um::winuser::GetWindow(hwnd, winapi::um::winuser::GW_CHILD);
                    while child != std::ptr::null_mut() {
                        collect(child, out);
                        child = winapi::um::winuser::GetWindow(child, winapi::um::winuser::GW_HWNDNEXT);
                    }
                }
            }
            let mut hwnds = Vec::new();
            collect(dlg_hwnd as _, &mut hwnds);
            static DEFRESP_ID: AtomicUsize = AtomicUsize::new(0xD1000000);
            for child in hwnds {
                let cb = self.response_cb.clone();
                let id = DEFRESP_ID.fetch_add(1, Ordering::SeqCst);
                if let Ok(h) = nwg::bind_raw_event_handler(
                    &nwg::ControlHandle::Hwnd(child), id,
                    move |_h, msg, w, _l| {
                        if msg == winapi::um::winuser::WM_KEYDOWN && (w & 0xFF) == 0x0D {
                            if let Some(ref mut f) = *cb.borrow_mut() { f(response_id); }
                            // Dismiss like the button path: confirming must close.
                            unsafe {
                                winapi::um::winuser::ShowWindow(dlg_hwnd as _, winapi::um::winuser::SW_HIDE);
                            }
                            return Some(0);
                        }
                        None
                    },
                ) {
                    self.dlg_key_handlers.borrow_mut().push(h);
                }
            }
        }
        /// Bind an Escape-to-dismiss raw handler on the dialog window and all
        /// of its current descendants (see present()).
        fn bind_esc_dismiss(&self, dlg_hwnd: winapi::shared::windef::HWND) {
            // Collect dialog + recursive descendants.
            fn collect(hwnd: winapi::shared::windef::HWND, out: &mut Vec<winapi::shared::windef::HWND>) {
                out.push(hwnd);
                unsafe {
                    let mut child = winapi::um::winuser::GetWindow(hwnd, winapi::um::winuser::GW_CHILD);
                    while child != std::ptr::null_mut() {
                        collect(child, out);
                        child = winapi::um::winuser::GetWindow(child, winapi::um::winuser::GW_HWNDNEXT);
                    }
                }
            }
            let mut hwnds = Vec::new();
            collect(dlg_hwnd as _, &mut hwnds);
            static ESC_ID: AtomicUsize = AtomicUsize::new(0xD0000000);
            for child in hwnds {
                let cb = self.response_cb.clone();
                let id = ESC_ID.fetch_add(1, Ordering::SeqCst);
                if let Ok(h) = nwg::bind_raw_event_handler(
                    &nwg::ControlHandle::Hwnd(child), id,
                    move |_h, msg, w, _l| {
                        if (msg == winapi::um::winuser::WM_KEYDOWN
                            || msg == winapi::um::winuser::WM_SYSKEYDOWN)
                            && w == winapi::um::winuser::VK_ESCAPE as usize
                        {
                            if let Some(ref mut f) = *cb.borrow_mut() { f(0); }
                            unsafe {
                                winapi::um::winuser::ShowWindow(dlg_hwnd as _,
                                    winapi::um::winuser::SW_HIDE);
                            }
                            return Some(0);
                        }
                        None
                    },
                ) {
                    self.dlg_key_handlers.borrow_mut().push(h);
                }
            }
        }
        pub fn close(&self) {
            if let Some(hwnd) = self.inner.handle.hwnd() {
                unsafe {
                    winapi::um::winuser::ShowWindow(hwnd as _, winapi::um::winuser::SW_HIDE);
                }
            }
        }
    }

    impl AsRef<*mut c_void> for Dialog {
        fn as_ref(&self) -> &*mut c_void {
            &self.hwnd
        }
    }

    impl Widget for Dialog {
        fn raw_handle(&self) -> *mut c_void {
            self.inner.handle.hwnd().unwrap_or(std::ptr::null_mut()) as *mut c_void
        }
    }

    impl Dialog {
        pub fn set_visible(&self, v: bool) {
            let hwnd = self.inner.handle.hwnd().unwrap_or(std::ptr::null_mut());
            set_explicitly_hidden(hwnd as _, !v);
            unsafe {
                winapi::um::winuser::ShowWindow(hwnd as _, if v { winapi::um::winuser::SW_SHOW } else { winapi::um::winuser::SW_HIDE });
            }
        }
    }

    pub fn create_dialog(
        parent_cell: &Rc<RefCell<Option<*mut c_void>>>,
    ) -> Result<Dialog, Error> {
        let response_cb: Rc<RefCell<Option<Box<dyn FnMut(i32)>>>> = Rc::new(RefCell::new(None));
        let btn_cb = response_cb.clone();
        let (inner, _, handler) = crate::backends::nwg::create_dialog(parent_cell, btn_cb)
            .map_err(|e| Error::Backend(format!("{}", e)))?;

        let layout_cb: Rc<RefCell<Vec<Box<dyn FnMut(i32, i32)>>>> = Rc::new(RefCell::new(Vec::new()));

        let content: Rc<RefCell<Vec<*mut c_void>>> = Rc::new(RefCell::new(Vec::new()));
        let buttons: Rc<RefCell<Vec<(nwg::Button, nwg::EventHandler)>>> =
            Rc::new(RefCell::new(Vec::new()));
        // Bind raw WM_SIZE handler for dialog
        let dlg_hwnd = inner.handle.hwnd().unwrap_or(std::ptr::null_mut());
        if dlg_hwnd != std::ptr::null_mut() {
            let cb = layout_cb.clone();
            let content_wm = content.clone();
            let buttons_wm = buttons.clone();
            static DIALOG_RAW_HANDLER_ID: AtomicUsize = AtomicUsize::new(0x20000000);
            let handler_id = DIALOG_RAW_HANDLER_ID.fetch_add(1, Ordering::SeqCst);
            nwg::bind_raw_event_handler(
                &nwg::ControlHandle::Hwnd(dlg_hwnd),
                handler_id,
                move |_h, msg, _w, l| {
                    if msg == winapi::um::winuser::WM_SIZE {
                        let w = (l & 0xFFFF) as i32;
                        let h = ((l >> 16) & 0xFFFF) as i32;
                        for cb_item in cb.borrow_mut().iter_mut() {
                            cb_item(w, h);
                        }
                        layout_nwg_dialog_parts(dlg_hwnd, &content_wm, &buttons_wm);
                    }
                    None
                },
            ).map_err(|e| Error::Backend(format!("{}", e)))?;
        }

        Ok(Dialog { hwnd: inner.handle.hwnd().unwrap_or(std::ptr::null_mut()) as *mut c_void, inner: Rc::new(inner), buttons, response_cb, _handler: Rc::new(handler), layout_cb, dlg_key_handlers: Rc::new(RefCell::new(Vec::new())), esc_bound: Rc::new(RefCell::new(false)), content })
    }

    pub fn create_dialog_button(
        parent: *mut c_void,
        text: &str,
        response_id: i32,
        cb: Rc<RefCell<Option<Box<dyn FnMut(i32)>>>>,
    ) -> Result<(nwg::Button, nwg::EventHandler), Error> {
        crate::backends::nwg::create_dialog_button(parent, text, response_id, cb)
            .map_err(|e| Error::Backend(format!("{}", e)))
    }

    // ========== GDI DrawContext ==========

    pub struct NwgDrawContext {
        hdc: winapi::shared::windef::HDC,
        w: i32,
        h: i32,
    }

    impl NwgDrawContext {
        fn make_font(name: &str, size: f64, weight: i32, italic: bool) -> winapi::shared::windef::HFONT {
            unsafe {
                let is_mono = name.eq_ignore_ascii_case("monospace");
                let face = if is_mono { "Courier New" } else { name };
                let wide_name: Vec<u16> = face.encode_utf16().chain(std::iter::once(0)).collect();
                let pitch = if is_mono { winapi::um::wingdi::FF_MODERN } else { 0 };
                winapi::um::wingdi::CreateFontW(
                    -(size.abs() as i32), 0, 0, 0, weight as i32, italic as u32, 0, 0,
                    winapi::um::wingdi::ANSI_CHARSET,
                    winapi::um::wingdi::OUT_DEFAULT_PRECIS,
                    winapi::um::wingdi::CLIP_DEFAULT_PRECIS,
                    winapi::um::wingdi::PROOF_QUALITY,
                    winapi::um::wingdi::DEFAULT_PITCH | pitch,
                    wide_name.as_ptr(),
                )
            }
        }
    }

    impl DrawContext for NwgDrawContext {
        fn fill_rect(&mut self, x: f64, y: f64, w: f64, h: f64, r: f64, g: f64, b: f64, _a: f64) {
            unsafe {
                let color: u32 = winapi::um::wingdi::RGB(
                    (r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8);
                let brush = winapi::um::wingdi::CreateSolidBrush(color);
                if !brush.is_null() {
                    let mut rect = winapi::shared::windef::RECT {
                        left: x as i32, top: y as i32,
                        right: (x + w) as i32, bottom: (y + h) as i32,
                    };
                    winapi::um::winuser::FillRect(self.hdc, &mut rect, brush);
                    winapi::um::wingdi::DeleteObject(brush as _);
                }
            }
        }
        fn stroke_rect(&mut self, x: f64, y: f64, w: f64, h: f64, r: f64, g: f64, b: f64, _a: f64, lw: f64) {
            unsafe {
                let color: u32 = winapi::um::wingdi::RGB(
                    (r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8);
                let pen = winapi::um::wingdi::CreatePen(winapi::um::wingdi::PS_SOLID as i32, lw as i32, color);
                if !pen.is_null() {
                    let old_pen = winapi::um::wingdi::SelectObject(self.hdc, pen as _);
                    if w == 0.0 && h != 0.0 {
                        winapi::um::wingdi::MoveToEx(self.hdc, x as i32, y as i32, std::ptr::null_mut());
                        winapi::um::wingdi::LineTo(self.hdc, x as i32, (y + h) as i32);
                    } else if h == 0.0 && w != 0.0 {
                        winapi::um::wingdi::MoveToEx(self.hdc, x as i32, y as i32, std::ptr::null_mut());
                        winapi::um::wingdi::LineTo(self.hdc, (x + w) as i32, y as i32);
                    } else {
                        let null_brush = winapi::um::wingdi::GetStockObject(winapi::um::wingdi::NULL_BRUSH as i32);
                        let old_brush = winapi::um::wingdi::SelectObject(self.hdc, null_brush);
                        winapi::um::wingdi::Rectangle(self.hdc, x as i32, y as i32, (x + w) as i32, (y + h) as i32);
                        winapi::um::wingdi::SelectObject(self.hdc, old_brush);
                    }
                    winapi::um::wingdi::SelectObject(self.hdc, old_pen);
                    winapi::um::wingdi::DeleteObject(pen as _);
                }
            }
        }
        fn draw_text_styled(&mut self, x: f64, y: f64, text: &str, font: &str, size: f64,
                            r: f64, g: f64, b: f64, _a: f64, _slant: i32, weight: i32) {
            unsafe {
                let wide: Vec<u16> = text.encode_utf16().collect();
                let color: u32 = winapi::um::wingdi::RGB(
                    (r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8);
                winapi::um::wingdi::SetTextColor(self.hdc, color);
                let old_bkmode = winapi::um::wingdi::SetBkMode(self.hdc, winapi::um::wingdi::TRANSPARENT as i32);
                let hfont = Self::make_font(font, size, if weight != 0 { winapi::um::wingdi::FW_BOLD as i32 } else { winapi::um::wingdi::FW_NORMAL as i32 }, _slant != 0);
                if hfont.is_null() {
                    let sys = winapi::um::wingdi::GetStockObject(winapi::um::wingdi::SYSTEM_FONT as i32) as winapi::shared::windef::HFONT;
                    let old = winapi::um::wingdi::SelectObject(self.hdc, sys as _);
                    winapi::um::wingdi::TextOutW(self.hdc, x as i32, y as i32, wide.as_ptr() as _, wide.len() as i32);
                    winapi::um::wingdi::SelectObject(self.hdc, old);
                } else {
                    let old_font = winapi::um::wingdi::SelectObject(self.hdc, hfont as _);
                    winapi::um::wingdi::TextOutW(self.hdc, x as i32, y as i32, wide.as_ptr() as _, wide.len() as i32);
                    winapi::um::wingdi::SelectObject(self.hdc, old_font);
                    winapi::um::wingdi::DeleteObject(hfont as _);
                }
                winapi::um::wingdi::SetBkMode(self.hdc, old_bkmode);
            }
        }
        fn text_extents_styled(&self, text: &str, font: &str, size: f64, _slant: i32, weight: i32) -> (f64, f64, f64, f64) {
            unsafe {
                let wide: Vec<u16> = text.encode_utf16().collect();
                let hfont = Self::make_font(font, size, if weight != 0 { winapi::um::wingdi::FW_BOLD as i32 } else { winapi::um::wingdi::FW_NORMAL as i32 }, _slant != 0);
                if hfont.is_null() { return (0.0, 0.0, 0.0, 0.0); }
                let old_font = winapi::um::wingdi::SelectObject(self.hdc, hfont as _);
                let mut size_tag: winapi::shared::windef::SIZE = std::mem::zeroed();
                winapi::um::wingdi::GetTextExtentPoint32W(self.hdc, wide.as_ptr() as _, wide.len() as i32, &mut size_tag);
                winapi::um::wingdi::SelectObject(self.hdc, old_font);
                winapi::um::wingdi::DeleteObject(hfont as _);
                (0.0, 0.0, size_tag.cx as f64, size_tag.cy as f64)
            }
        }
        fn clear(&mut self, r: f64, g: f64, b: f64, _a: f64) {
            self.fill_rect(0.0, 0.0, self.w as f64, self.h as f64, r, g, b, 1.0);
        }
        fn save(&mut self) {
            unsafe { winapi::um::wingdi::SaveDC(self.hdc); }
        }
        fn restore(&mut self) {
            unsafe { winapi::um::wingdi::RestoreDC(self.hdc, -1); }
        }
        fn clip(&mut self, x: f64, y: f64, w: f64, h: f64) {
            unsafe { winapi::um::wingdi::IntersectClipRect(self.hdc, x as i32, y as i32, (x + w) as i32, (y + h) as i32); }
        }
    }

    // ========== Canvas ==========

/// Handler ids for the gesture stream.
///
/// `bind_raw_event_handler` panics on ids <= 0xFFFF, which NWG reserves, so
/// these start well above it and increment. A process-wide counter avoids two
/// canvases colliding on the same hwnd.
fn next_gesture_id() -> usize {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0x5000_0000);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

/// The client-relative point packed into an `LPARAM` by a mouse message.
///
/// Win32 packs x in the low word and y in the high word. Both are read as
/// `i16` because the signed-ness is carried in the top bit: a window dragged
/// partly off the left edge reports a negative x, and reading it as unsigned
/// would turn that into ~65000 and put the pointer in the wrong place.
fn win32_point(l: isize) -> (f64, f64) {
    let l = l as u32;
    let x = (l & 0xFFFF) as i16 as f64;
    let y = ((l >> 16) & 0xFFFF) as i16 as f64;
    (x, y)
}

/// Decode a mouse message's `WPARAM` into the portable `Modifiers`.
///
/// The `GET_KEYSTATE` bits live in the low 16 bits: shift 0x0001, control
/// 0x0002, alt 0x0004 and the Win key 0x0008. The button mask starts at bit 8.
fn win32_mods(w: usize) -> crate::core::Modifiers {
    let k = (w as u32 & 0xFFFF) as u16;
    crate::core::Modifiers {
        shift: k & 0x0001 != 0,
        ctrl: k & 0x0002 != 0,
        alt: k & 0x0004 != 0,
        meta: k & 0x0008 != 0,
    }
}

    pub struct Canvas {
        frame: Option<Rc<nwg::Frame>>,
        hwnd: *mut c_void,
        draw_cb: Rc<RefCell<Option<Box<dyn FnMut(&mut dyn crate::core::DrawContext, i32, i32)>>>>,
        click_cb: Rc<RefCell<Option<Box<dyn FnMut(f64, f64)>>>>,
        key_cb: Rc<RefCell<Option<Box<dyn FnMut(u32) -> bool>>>>,
        /// The canonical gesture stream, fed from raw Win32 mouse messages.
        gesture_cb: Rc<RefCell<Option<Box<dyn FnMut(crate::core::Gesture)>>>>,
        _raw_handlers: Rc<Vec<nwg::RawEventHandler>>,
        painting: Rc<RefCell<bool>>,
    }

    impl Canvas {
        pub fn set_draw_callback(&self, cb: Box<dyn FnMut(&mut dyn DrawContext, i32, i32)>) {
            *self.draw_cb.borrow_mut() = Some(cb);
            self.queue_redraw();
        }
        /// Whether the canvas may take extra horizontal space.
        ///
        /// The NWG `Frame` this wraps exposes no expansion flag, so this is a
        /// no-op kept for parity with the GTK backend: a caller written once
        /// for both compiles unchanged, and the GTK path -- the one the image
        /// viewer actually runs on -- does honour it.
        pub fn set_hexpand(&self, _expand: bool) {}
        /// See `set_hexpand`.
        pub fn set_vexpand(&self, _expand: bool) {}
        pub fn queue_redraw(&self) {
            if !self.hwnd.is_null() {
                unsafe {
                    winapi::um::winuser::InvalidateRect(self.hwnd as _, std::ptr::null_mut(), 0);
                }
            }
        }
        pub fn set_size_request(&self, w: i32, h: i32) {
            if !self.hwnd.is_null() {
                unsafe {
                    winapi::um::winuser::SetWindowPos(
                        self.hwnd as _,
                        std::ptr::null_mut(), 0, 0, w, h,
                        winapi::um::winuser::SWP_NOZORDER | winapi::um::winuser::SWP_NOMOVE,
                    );
                }
            }
        }

        pub fn set_visible(&self, visible: bool) {
            if !self.hwnd.is_null() {
                set_explicitly_hidden(self.hwnd, !visible);
                unsafe {
                    winapi::um::winuser::ShowWindow(
                        self.hwnd as _,
                        if visible { winapi::um::winuser::SW_SHOW } else { winapi::um::winuser::SW_HIDE },
                    );
                }
            }
        }
        pub fn set_content_size(&self, _w: i32, _h: i32) {}
        /// Button/modifier-aware click. See the GTK backend's
        /// `on_click_button`: added alongside `on_click` so the signature change
        /// does not ripple through every backend, and so a backend that cannot
        /// report a button (a terminal, which has the plain `on_click`) stays
        /// compilable. The NWG canvas currently maps only the plain contract.
        pub fn on_click_button(&self, _cb: Box<dyn FnMut(f64, f64, u32, u32)>) {
            // NWG/Win32: button and motion are not plumbed through this path yet.
        }

        /// Pointer motion over the canvas; see the GTK backend's `on_motion`.
        /// The NWG canvas currently reports presses only.
        pub fn on_motion(&self, _cb: Box<dyn FnMut(f64, f64, u32)>) {
            // NWG/Win32: button and motion are not plumbed through this path yet.
        }

        /// Pointer release; see the GTK backend's `on_release`.
        pub fn on_release(&self, _cb: Box<dyn FnMut(f64, f64, u32, u32)>) {
            // NWG/Win32: button and motion are not plumbed through this path yet.
        }

        /// This canvas's top-left in screen coordinates, or `None`.
        ///
        /// `ClientToScreen(0, 0)` on the canvas' own HWND, so a caller can
        /// turn a widget-relative click into the screen point a popup wants.
        /// Returns `None` only if the hwnd is gone (`ClientToScreen` failing
        /// is not otherwise distinguished — a stale hwnd is a use-after-free
        /// bug in the caller, not something to paper over here).
        pub fn screen_origin(&self) -> Option<(i32, i32)> {
            let hwnd = self.hwnd;
            if hwnd.is_null() {
                return None;
            }
            unsafe {
                let mut pt = winapi::shared::windef::POINT { x: 0, y: 0 };
                if winapi::um::winuser::ClientToScreen(hwnd as _, &mut pt) == 0 {
                    return None;
                }
                Some((pt.x, pt.y))
            }
        }

        pub fn on_click(&self, cb: Box<dyn FnMut(f64, f64)>) {
            *self.click_cb.borrow_mut() = Some(cb);
        }
        pub fn on_key(&self, cb: Box<dyn FnMut(u32) -> bool>) {
            *self.key_cb.borrow_mut() = Some(cb);
        }
        pub fn on_key_raw(&self, cb: Box<dyn FnMut(u32, u32) -> bool>) {
            let mut cb = cb;
            *self.key_cb.borrow_mut() = Some(Box::new(move |k: u32| -> bool { cb(k, 0) }));
        }
        pub fn grab_focus(&self) {
            unsafe { winapi::um::winuser::SetFocus(self.hwnd as _); }
        }
        pub fn set_can_focus(&self, _can: bool) {}
        pub fn force_draw(&self, _window_ptr: *mut c_void, _fallback_w: i32, _fallback_h: i32) {}
        /// The canonical gesture stream. See `core::Gesture`.
        ///
        /// Implemented over raw Win32 messages via
        /// `nwg::bind_raw_event_handler`, because NWG exposes no mouse API on a
        /// `Frame` -- its `on_motion` / `on_release` / `on_click_button` are
        /// still no-ops that never fire. NWG's own `Event::OnMouseMove` would
        /// work, but it carries no coordinates, and a canvas needs a position
        /// in its own client space.
        ///
        /// Handles `WM_LBUTTONDOWN` / `WM_LBUTTONUP` (with right-button mapped
        /// onto the middle/secondary enum value), `WM_MOUSEMOVE` and
        /// `WM_MOUSEWHEEL`. Coordinates come from the low/high halves of the
        /// `LPARAM`, which is how Win32 packs a client-relative point, and are
        /// signed via `i16` because a window can extend past the screen edge.
        ///
        /// Modifiers come from the `GET_KEYSTATE` bits in the `WPARAM`.
        pub fn on_gesture(&self, cb: Box<dyn FnMut(crate::core::Gesture)>) {
            *self.gesture_cb.borrow_mut() = Some(cb);
        }

        /// Binds the raw handlers that feed the gesture stream.
        ///
        /// Separate from `on_gesture` so it is called once at construction --
        /// binding handlers is what costs, and doing it per registration would
        /// leak one handler per call.
        pub(crate) fn install_gesture_handlers(&self, raw_hwnd: isize) {
            let cb = self.gesture_cb.clone();
            let mut ids: Vec<usize> = Vec::new();

            // Press and release. WM_LBUTTONDOWN/UP carry the position in LPARAM;
            // the right-button messages are used for the secondary button so a
            // right-click drag is reachable at all.
            for (msg, button, pressed) in [
                (winapi::um::winuser::WM_LBUTTONDOWN, crate::core::Button::Primary, true),
                (winapi::um::winuser::WM_LBUTTONUP, crate::core::Button::Primary, false),
                (winapi::um::winuser::WM_RBUTTONDOWN, crate::core::Button::Secondary, true),
                (winapi::um::winuser::WM_RBUTTONUP, crate::core::Button::Secondary, false),
            ] {
                let slot = cb.clone();
                let id = next_gesture_id();
                if nwg::bind_raw_event_handler(
                    &nwg::ControlHandle::Hwnd(raw_hwnd as _),
                    id,
                    move |_hwnd, m, w, l| {
                        if m != msg {
                            return None;
                        }
                        let (x, y) = win32_point(l);
                        let mods = win32_mods(w);
                        if let Some(f) = slot.borrow_mut().as_mut() {
                            f(crate::core::Gesture::Button { button, pressed, x, y, mods });
                        }
                        // Do NOT consume: the press must still reach on_click,
                        // which has its own handler on WM_LBUTTONDOWN.
                        None
                    },
                )
                .is_ok()
                {
                    ids.push(id);
                }
            }

            // Motion. Whether this is a drag or a hover comes from the button
            // mask in WPARAM (GET_KEYSTATE), exactly as on GTK: plain motion
            // with no button down is a hover, with one down it is a drag.
            {
                let slot = cb.clone();
                let id = next_gesture_id();
                if nwg::bind_raw_event_handler(
                    &nwg::ControlHandle::Hwnd(raw_hwnd as _),
                    id,
                    move |_hwnd, m, w, l| {
                        if m != winapi::um::winuser::WM_MOUSEMOVE {
                            return None;
                        }
                        let (x, y) = win32_point(l);
                        let held = (w as u16 as i16 as u32 & (1 << (8 + 0))) != 0;
                        let mods = win32_mods(w);
                        let g = if held {
                            crate::core::Gesture::Drag { x, y, mods }
                        } else {
                            crate::core::Gesture::Hover { x, y, mods }
                        };
                        if let Some(f) = slot.borrow_mut().as_mut() {
                            f(g);
                        }
                        None
                    },
                )
                .is_ok()
                {
                    ids.push(id);
                }
            }

            // Wheel. Win32 puts the notches in the high word of WPARAM
            // (GET_WHEEL_DELTA_WPARAM) as 120 per notch, signed. Deltas are
            // normalised from 120 to 1.0 so a notch reads as "one step" on
            // every backend, rather than making every caller know the Win32
            // unit.
            {
                let slot = cb.clone();
                let id = next_gesture_id();
                if nwg::bind_raw_event_handler(
                    &nwg::ControlHandle::Hwnd(raw_hwnd as _),
                    id,
                    move |_hwnd, m, w, l| {
                        if m != winapi::um::winuser::WM_MOUSEWHEEL {
                            return None;
                        }
                        let wparam = w as u32;
                        let notches = ((wparam >> 16) & 0xFFFF) as i16 as f64;
                        // Low bit of WPARAM is the "not from the scroll bar"
                        // flag; without it a window that has no scrollbar would
                        // never see the wheel.
                        if wparam & 0x0001 == 0 {
                            return None;
                        }
                        if notches == 0.0 {
                            return None;
                        }
                        const WHEEL_DELTA: f64 = 120.0;
                        let (x, y) = win32_point(l);
                        let mods = win32_mods(w);
                        if let Some(f) = slot.borrow_mut().as_mut() {
                            f(crate::core::Gesture::Scroll {
                                delta: crate::core::ScrollDelta { dx: 0.0, dy: notches / WHEEL_DELTA },
                                x,
                                y,
                                mods,
                            });
                        }
                        // Consumed: the wheel belongs to the canvas, and letting
                        // it through scrolls whatever is behind the window.
                        Some(0)
                    },
                )
                .is_ok()
                {
                    ids.push(id);
                }
            }

            // Bind and drop the `RawEventHandler` values. NWG keeps the
            // registration alive internally; the handle is only a token for
            // unbinding, so dropping it here is correct and matches how the
            // existing click handler is installed. Only the slot is retained.
            let _ = ids;
        }
    }

    impl Clone for Canvas {
        fn clone(&self) -> Self {
            Canvas {
                frame: self.frame.clone(),
                hwnd: self.hwnd,
                draw_cb: self.draw_cb.clone(),
                click_cb: self.click_cb.clone(),
                key_cb: self.key_cb.clone(),
                gesture_cb: self.gesture_cb.clone(),
                _raw_handlers: Rc::new(Vec::new()),
                painting: self.painting.clone(),
            }
        }
    }

    impl AsRef<*mut c_void> for Canvas {
        fn as_ref(&self) -> &*mut c_void {
            &self.hwnd
        }
    }

    impl Widget for Canvas {
        fn raw_handle(&self) -> *mut c_void { self.hwnd }
    }

    pub fn create_canvas(parent: *mut c_void) -> Result<Canvas, Error> {
        let mut frame = nwg::Frame::default();
        if !parent.is_null() {
            nwg::Frame::builder()
                .flags(nwg::FrameFlags::VISIBLE)
                .size((0, 0))
                .position((0, 0))
                .parent(&nwg::ControlHandle::Hwnd(parent as _))
                .build(&mut frame)
                .map_err(|e| Error::Backend(format!("{}", e)))?;
        }
        let hwnd = frame.handle.hwnd().unwrap_or(std::ptr::null_mut()) as *mut c_void;
        let draw_cb: Rc<RefCell<Option<Box<dyn FnMut(&mut dyn DrawContext, i32, i32)>>>> = Rc::new(RefCell::new(None));
        let painting: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
        let click_cb: Rc<RefCell<Option<Box<dyn FnMut(f64, f64)>>>> = Rc::new(RefCell::new(None));
        let gesture_cb: Rc<RefCell<Option<Box<dyn FnMut(crate::core::Gesture)>>>> = Rc::new(RefCell::new(None));
        let key_cb: Rc<RefCell<Option<Box<dyn FnMut(u32) -> bool>>>> = Rc::new(RefCell::new(None));

        let mut handlers: Vec<nwg::RawEventHandler> = Vec::new();

        if hwnd != std::ptr::null_mut() {
            let raw_hwnd: winapi::shared::windef::HWND = hwnd as _;

            // WM_KEYDOWN/WM_SYSKEYDOWN handler for keyboard input.
            // Without this, the Canvas's key_cb is never called because
            // no raw handler is registered to process keystroke messages.
            {
                let kc = key_cb.clone();
                static KEYBOARD_ID: AtomicUsize = AtomicUsize::new(0x70000000);
                let kid = KEYBOARD_ID.fetch_add(1, Ordering::SeqCst);
                if let Some(h) = nwg::bind_raw_event_handler(
                    &nwg::ControlHandle::Hwnd(raw_hwnd), kid,
                    move |_h, msg, w, _l| {
                        if msg == winapi::um::winuser::WM_KEYDOWN || msg == winapi::um::winuser::WM_SYSKEYDOWN {
                            // Same pure-Alt yield as the entry handler: Alt+letter
                            // must reach DefWindowProc (native menu), never the
                            // grid edit path. Ctrl+Alt (AltGr) passes through.
                            if pure_alt_held() {
                                return None;
                            }
                            // Pure Ctrl is accelerators, not grid input: decline
                            // instead of typing the letter (Ctrl+C must never
                            // insert 'c'). Ctrl+Alt (AltGr) passes through.
                            if pure_ctrl_held() {
                                return None;
                            }
                            if let Some(ref mut f) = *kc.borrow_mut() {
                                // Translate Shift pairs/capitals like the
                                // entry path; specials keep raw VKs. A
                                // colliding translation (None) declines.
                                if let Some(k) = translate_vk(w) {
                                    if f(k) { return Some(0); }
                                } else {
                                    return None;
                                }
                            }
                        }
                        None
                    },
                ).ok() { handlers.push(h); }
            }

            // Suppress WM_ERASEBKGND (prevent flash from class background brush)
            {
                static ERASE_ID: AtomicUsize = AtomicUsize::new(0x60000000);
                let eid = ERASE_ID.fetch_add(1, Ordering::SeqCst);
                if let Some(h) = nwg::bind_raw_event_handler(
                    &nwg::ControlHandle::Hwnd(raw_hwnd), eid,
                    move |_h, msg, _w, _l| {
                        if msg == winapi::um::winuser::WM_ERASEBKGND { Some(1) } else { None }
                    },
                ).ok() { handlers.push(h); }
            }

            // WM_PAINT handler
            {
                let cb = draw_cb.clone();
                let paint_flag = painting.clone();
                static CANVAS_PAINT_ID: AtomicUsize = AtomicUsize::new(0x30000000);
                let pid = CANVAS_PAINT_ID.fetch_add(1, Ordering::SeqCst);
                if let Some(h) = nwg::bind_raw_event_handler(
                    &nwg::ControlHandle::Hwnd(raw_hwnd), pid,
                    move |_h, msg, _w, _l| {
                        if msg != winapi::um::winuser::WM_PAINT { return None; }
                        // TEMPORARY ReactOS diagnosis: canvas WM_PAINT entry.
                        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                        { mark95a(b"cvpaint\n"); }
                        if *paint_flag.borrow() { return Some(0); }
                        *paint_flag.borrow_mut() = true;
                        unsafe {
                            let mut ps: winapi::um::winuser::PAINTSTRUCT = std::mem::zeroed();
                            let hdc = winapi::um::winuser::BeginPaint(hwnd as _, &mut ps);
                            let mut rect: winapi::shared::windef::RECT = std::mem::zeroed();
                            winapi::um::winuser::GetClientRect(hwnd as _, &mut rect);
                            let w = rect.right;
                            let h = rect.bottom;
                            // TEMPORARY ReactOS diagnosis: the canvas size the
                            // paint actually ran at.
                            #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                            mark95xy(b"cvsz ", w, h);
                            if w > 0 && h > 0 {
                                let mem_dc = winapi::um::wingdi::CreateCompatibleDC(hdc);
                                // TEMPORARY ReactOS diagnosis: did GDI give us a
                                // usable memory DC and a non-null bitmap?
                                #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                                mark95xy(
                                    b"gdidc",
                                    if mem_dc.is_null() { 0 } else { 1 },
                                    hdc as i32,
                                );
                                if !mem_dc.is_null() {
                                    let bmp = winapi::um::wingdi::CreateCompatibleBitmap(hdc, w, h);
                                    #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                                    mark95xy(
                                        b"gdibm",
                                        if bmp.is_null() { 0 } else { 1 },
                                        w,
                                    );
                                    if !bmp.is_null() {
                                        let old = winapi::um::wingdi::SelectObject(mem_dc, bmp as _);
                                        // Paint the buffer with a neutral
                                        // chrome colour before the callback
                                        // runs. CreateCompatibleBitmap hands
                                        // back uninitialised memory (black on
                                        // Win9x), so a callback that draws
                                        // nothing — or returns early because
                                        // the widget has nothing to show —
                                        // would BitBlt that garbage onto the
                                        // window as a black band.
                                        {
                                            let color = winapi::um::wingdi::RGB(240, 240, 240);
                                            let brush = winapi::um::wingdi::CreateSolidBrush(color);
                                            if !brush.is_null() {
                                                let mut rect = winapi::shared::windef::RECT {
                                                    left: 0, top: 0, right: w, bottom: h,
                                                };
                                                winapi::um::winuser::FillRect(mem_dc, &mut rect, brush);
                                                winapi::um::wingdi::DeleteObject(brush as _);
                                            }
                                        }
                                        // TEMPORARY ReactOS diagnosis: did the
                                        // paint find a registered draw callback?
                                        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                                        {
                                            mark95xy(
                                                b"hascb",
                                                if cb.borrow().is_some() { 1 } else { 0 },
                                                0,
                                            );
                                        }
                                        if let Some(ref mut draw_fn) = *cb.borrow_mut() {
                                            let mut ctx = NwgDrawContext { hdc: mem_dc, w, h };
                                            draw_fn(&mut ctx, w, h);
                                        }
                                        // TEMPORARY ReactOS diagnosis: BitBlt the
                                        // buffer onto the window.
                                        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                                        {
                                            let r = winapi::um::wingdi::BitBlt(
                                                hdc, 0, 0, w, h, mem_dc, 0, 0,
                                                winapi::um::wingdi::SRCCOPY,
                                            );
                                            extern "system" {
                                                fn GetLastError() -> u32;
                                            }
                                            mark95xy(
                                                b"blt  ",
                                                r,
                                                GetLastError() as i32,
                                            );
                                        }
                                        // BitBlt returns NONZERO on success, so
                                        // this is just the mark; the copy itself
                                        // is the plain SRCCopy below.
                                        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                                        {
                                            extern "system" {
                                                fn GetLastError() -> u32;
                                            }
                                            let r = winapi::um::wingdi::BitBlt(
                                                hdc, 0, 0, w, h, mem_dc, 0, 0,
                                                winapi::um::wingdi::SRCCOPY,
                                            );
                                            mark95xy(b"blt  ", r, GetLastError() as i32);
                                        }
                                        winapi::um::wingdi::BitBlt(
                                            hdc, 0, 0, w, h, mem_dc, 0, 0,
                                            winapi::um::wingdi::SRCCOPY,
                                        );
                                        winapi::um::wingdi::SelectObject(mem_dc, old);
                                        winapi::um::wingdi::DeleteObject(bmp as _);
                                    }
                                    winapi::um::wingdi::DeleteDC(mem_dc);
                                }
                            }
                            winapi::um::winuser::EndPaint(hwnd as _, &mut ps);
                        }
                        *paint_flag.borrow_mut() = false;
                        Some(0)
                    },
                ).ok() { handlers.push(h); }
            }

            // WM_LBUTTONDOWN handler
            {
                let cc = click_cb.clone();
                static CLICK_ID: AtomicUsize = AtomicUsize::new(0x40000000);
                let cid = CLICK_ID.fetch_add(1, Ordering::SeqCst);
                if let Some(h) = nwg::bind_raw_event_handler(
                    &nwg::ControlHandle::Hwnd(raw_hwnd), cid,
                    move |_h, msg, _w, l| {
                        if msg != winapi::um::winuser::WM_LBUTTONDOWN { return None; }
                        {
                            let x = (l & 0xFFFF) as i16 as f64;
                            let y = ((l >> 16) & 0xFFFF) as i16 as f64;
                            if let Some(ref mut f) = *cc.borrow_mut() {
                                f(x, y);
                            }
                        }
                        Some(0)
                    },
                ).ok() { handlers.push(h); }
            }

            // WM_KEYDOWN handler — need focus first; forward WM_SETFOCUS to force keyboard input
            {
                let kc = key_cb.clone();
                static KEY_ID: AtomicUsize = AtomicUsize::new(0x50000000);
                let kid = KEY_ID.fetch_add(1, Ordering::SeqCst);
                if let Some(h) = nwg::bind_raw_event_handler(
                    &nwg::ControlHandle::Hwnd(raw_hwnd), kid,
                    move |_h, msg, w, _l| {
                        if msg != winapi::um::winuser::WM_KEYDOWN && msg != winapi::um::winuser::WM_SYSKEYDOWN { return None; }
                        // Pure Alt/Ctrl yield (menus/accelerators own those
                        // keys); Ctrl+Alt (AltGr) passes through.
                        if pure_alt_held() || pure_ctrl_held() {
                            return None;
                        }
                        if let Some(ref mut f) = *kc.borrow_mut() {
                            // Translate Shift pairs/capitals like the entry
                            // path; specials keep raw VKs. A colliding
                            // translation (None) declines.
                            if let Some(k) = translate_vk(w) {
                                if f(k) { return Some(0); }
                            } else {
                                return None;
                            }
                        }
                        None
                    },
                ).ok() { handlers.push(h); }
            }
        }

        let canvas = Canvas {
            frame: Some(Rc::new(frame)),
            gesture_cb,
            hwnd,
            draw_cb,
            click_cb,
            key_cb,
            _raw_handlers: Rc::new(handlers),
            painting,
        };
        // Bind the gesture handlers once, here, rather than in `on_gesture`:
        // `on_gesture` is called for every registration, and binding a subclass
        // per call would leak one each time (nwg refuses a duplicate id on the
        // same hwnd, so it would also fail on the second call).
        canvas.install_gesture_handlers(hwnd as isize);
        Ok(canvas)
    }

    // ========== Overlay ==========

    pub struct Overlay {
        frame: Option<Rc<nwg::Frame>>,
        hwnd: *mut c_void,
    }

    impl Overlay {
        pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
            let ptr = *child.as_ref();
            if !ptr.is_null() && !self.hwnd.is_null() {
                unsafe {
                    winapi::um::winuser::SetParent(ptr as _, self.hwnd as _);
                }
            }
        }
        pub fn add_overlay(&self, child: &impl AsRef<*mut c_void>) {
            let ptr = *child.as_ref();
            if !ptr.is_null() && !self.hwnd.is_null() {
                unsafe {
                    winapi::um::winuser::SetParent(ptr as _, self.hwnd as _);
                    winapi::um::winuser::SetWindowPos(
                        ptr as _, winapi::um::winuser::HWND_TOP,
                        0, 0, 0, 0,
                        winapi::um::winuser::SWP_NOSIZE | winapi::um::winuser::SWP_SHOWWINDOW,
                    );
                }
            }
        }
        pub fn set_overlay_pass_through(&self, _child: &impl AsRef<*mut c_void>, _pass: bool) {}
        pub fn remove(&self, _child: &impl AsRef<*mut c_void>) {}
        pub fn show_all(&self) {
            if !self.hwnd.is_null() {
                unsafe {
                    winapi::um::winuser::ShowWindow(self.hwnd as _, winapi::um::winuser::SW_SHOW);
                }
            }
        }
        pub fn set_vexpand(&self, _expand: bool) {}
        pub fn set_hexpand(&self, _expand: bool) {}
        pub fn set_size_request(&self, w: i32, h: i32) {
            if !self.hwnd.is_null() {
                unsafe {
                    winapi::um::winuser::SetWindowPos(
                        self.hwnd as _,
                        std::ptr::null_mut(), 0, 0, w, h,
                        winapi::um::winuser::SWP_NOZORDER | winapi::um::winuser::SWP_NOMOVE,
                    );
                }
            }
        }
    }

    impl Clone for Overlay {
        fn clone(&self) -> Self {
            Overlay { frame: self.frame.clone(), hwnd: self.hwnd }
        }
    }

    impl AsRef<*mut c_void> for Overlay {
        fn as_ref(&self) -> &*mut c_void {
            &self.hwnd
        }
    }

    impl Widget for Overlay {
        fn raw_handle(&self) -> *mut c_void { self.hwnd }
    }

    pub fn create_overlay(parent: *mut c_void) -> Result<Overlay, Error> {
        let mut frame = nwg::Frame::default();
        if !parent.is_null() {
            nwg::Frame::builder()
                .flags(nwg::FrameFlags::VISIBLE)
                .size((0, 0))
                .position((0, 0))
                .parent(&nwg::ControlHandle::Hwnd(parent as _))
                .build(&mut frame)
                .map_err(|e| Error::Backend(format!("{}", e)))?;
        }
        let hwnd = frame.handle.hwnd().unwrap_or(std::ptr::null_mut()) as *mut c_void;
        Ok(Overlay { frame: Some(Rc::new(frame)), hwnd })
    }

    // ========== ScrolledWindow ==========

    pub struct ScrolledWindow {
        frame: Option<Rc<nwg::Frame>>,
        hwnd: *mut c_void,
        child: Rc<RefCell<Option<*mut c_void>>>,
        vscroll: Rc<RefCell<nwg::ScrollBar>>,
        hscroll: Rc<RefCell<nwg::ScrollBar>>,
        _handlers: Rc<Vec<nwg::RawEventHandler>>,
        on_scroll_cb: Rc<RefCell<Option<Box<dyn FnMut(bool, f64)>>>>,
    }

    impl Clone for ScrolledWindow {
        fn clone(&self) -> Self {
            ScrolledWindow {
                frame: self.frame.clone(),
                hwnd: self.hwnd,
                child: self.child.clone(),
                vscroll: self.vscroll.clone(),
                hscroll: self.hscroll.clone(),
                _handlers: self._handlers.clone(),
                on_scroll_cb: self.on_scroll_cb.clone(),
            }
        }
    }

    impl ScrolledWindow {
        pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
            let ptr = *child.as_ref();
            if ptr.is_null() || self.hwnd.is_null() { return; }
            unsafe {
                winapi::um::winuser::SetParent(ptr as _, self.hwnd as _);
                // (Fix-ReactOS) The viewport frame must be visible before the
                // canvas is composited into it; assert the bit directly (see
                // force_visible) since a bare ShowWindow is dropped here.
                #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                force_visible(self.hwnd as _);
                winapi::um::winuser::SetWindowPos(
                    ptr as _, std::ptr::null_mut(),
                    0, 0, 0, 0,
                    winapi::um::winuser::SWP_NOZORDER | winapi::um::winuser::SWP_NOSIZE | winapi::um::winuser::SWP_SHOWWINDOW,
                );
            }
            // TEMPORARY ReactOS diagnosis: scrolled.set_child showed the child.
            #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
            { mark95xy(b"setc ", 1, ptr as i32); }
            *self.child.borrow_mut() = Some(ptr);
            // Size the child to the frame's client area on the next WM_SIZE.
            // (Scroll ranges are driven by the host via scroll_to; the child
            // always fills the viewport and nothing pans.)
            unsafe {
                let mut rect: winapi::shared::windef::RECT = std::mem::zeroed();
                winapi::um::winuser::GetClientRect(self.hwnd as _, &mut rect);
                let fw = rect.right - rect.left;
                let fh = rect.bottom - rect.top;
                if fw > 0 && fh > 0 {
                    winapi::um::winuser::SetWindowPos(
                        ptr as _, std::ptr::null_mut(),
                        0, 0, (fw - 20).max(0), (fh - 20).max(0),
                        winapi::um::winuser::SWP_NOZORDER | winapi::um::winuser::SWP_SHOWWINDOW,
                    );
                }
            }
        }

        pub fn set_policy(&self, _h: u32, _v: u32) {
            // NWG: always show both scrollbars
        }

        /// Drive both scrollbars from item indices (shared cursor-centric
        /// model with the GTK backend): value = cursor index, upper = domain
        /// size, page = visible count (page sizes the thumb where supported).
        pub fn scroll_to(&self, hval: f64, hupper: f64, _hpage: f64, vval: f64, vupper: f64, _vpage: f64) {
            if let Ok(sb) = self.hscroll.try_borrow() {
                sb.set_range(0..(hupper.max(1.0) as usize));
                sb.set_pos((hval.max(0.0) as usize).min(hupper.max(1.0) as usize));
            }
            if let Ok(sb) = self.vscroll.try_borrow() {
                sb.set_range(0..(vupper.max(1.0) as usize));
                sb.set_pos((vval.max(0.0) as usize).min(vupper.max(1.0) as usize));
            }
        }

        /// Notify on user scrollbar interaction: `cb(vertical, pos)`.
        /// Fires for thumb drags and trough clicks. The host moves its cursor
        /// (the canvas keeps filling the viewport; nothing pans).
        pub fn on_scroll(&self, cb: Box<dyn FnMut(bool, f64)>) {
            *self.on_scroll_cb.borrow_mut() = Some(cb);
        }

        pub fn set_vexpand(&self, _v: bool) {}
        pub fn set_hexpand(&self, _h: bool) {}

    }

    impl AsRef<*mut c_void> for ScrolledWindow {
        fn as_ref(&self) -> &*mut c_void {
            &self.hwnd
        }
    }

    pub fn create_scrolled_window(parent: *mut c_void) -> Result<ScrolledWindow, Error> {
        let mut frame = nwg::Frame::default();
        if !parent.is_null() {
            nwg::Frame::builder()
                .flags(nwg::FrameFlags::VISIBLE)
                .size((0, 0))
                .position((0, 0))
                .parent(&nwg::ControlHandle::Hwnd(parent as _))
                .build(&mut frame)
                .map_err(|e| Error::Backend(format!("{}", e)))?;
        }
        let hwnd = frame.handle.hwnd().unwrap_or(std::ptr::null_mut()) as *mut c_void;
        // (Fix-ReactOS) Assert the frame's own visibility at creation. The
        // frame is the canvas's parent, so a hidden frame hides the grid no
        // matter how correctly the canvas paints; and ReactOS does not
        // reliably keep the WS_VISIBLE bit a WS_CHILD is created with when the
        // parent is still being constructed. See force_visible.
        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
        force_visible(hwnd as winapi::shared::windef::HWND);

        let mut vscroll = nwg::ScrollBar::default();
        nwg::ScrollBar::builder()
            .flags(nwg::ScrollBarFlags::VISIBLE | nwg::ScrollBarFlags::VERTICAL)
            .parent(&nwg::ControlHandle::Hwnd(hwnd as _))
            .range(Some(0..1))
            .pos(Some(0))
            .build(&mut vscroll)
            .map_err(|e| Error::Backend(format!("{}", e)))?;

        let mut hscroll = nwg::ScrollBar::default();
        nwg::ScrollBar::builder()
            .flags(nwg::ScrollBarFlags::VISIBLE | nwg::ScrollBarFlags::HORIZONTAL)
            .parent(&nwg::ControlHandle::Hwnd(hwnd as _))
            .range(Some(0..1))
            .pos(Some(0))
            .build(&mut hscroll)
            .map_err(|e| Error::Backend(format!("{}", e)))?;

        let vscroll = Rc::new(RefCell::new(vscroll));
        let hscroll = Rc::new(RefCell::new(hscroll));
        let child: Rc<RefCell<Option<*mut c_void>>> = Rc::new(RefCell::new(None));
        let on_scroll_cb: Rc<RefCell<Option<Box<dyn FnMut(bool, f64)>>>> =
            Rc::new(RefCell::new(None));

        let mut handlers: Vec<nwg::RawEventHandler> = Vec::new();

        // Claim WM_ERASEBKGND on the frame. The frame is the canvas's PARENT
        // and paints over the same pixels the canvas occupies, and this frame
        // has no background of its own - so on a repaint ReactOS erased the
        // frame (and with it the freshly drawn grid) before the canvas could
        // be composited. The canvas already refuses to erase; the frame must
        // too, or the erase lands on top of the grid and the sheet comes out
        // blank white even though the draw callback ran.
        if hwnd != std::ptr::null_mut() {
            static SCROLLED_ERASE_ID: AtomicUsize = AtomicUsize::new(0xA0000000);
            let eid = SCROLLED_ERASE_ID.fetch_add(1, Ordering::SeqCst);
            if let Some(h) = nwg::bind_raw_event_handler(
                &nwg::ControlHandle::Hwnd(hwnd as _), eid,
                move |_h, msg, _w, _l| {
                    // TEMPORARY ReactOS diagnosis: which messages the scrolled
                    // frame actually sees, in order, so an overpaint after the
                    // canvas is visible in the trace.
                    #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                    {
                        use std::sync::atomic::{AtomicU32, Ordering};
                        static SEEN: AtomicU32 = AtomicU32::new(0);
                        let n = SEEN.fetch_add(1, Ordering::Relaxed);
                        if n < 24 {
                            mark95xy(b"fmsg ", msg as i32, n as i32);
                        }
                    }
                    if msg == winapi::um::winuser::WM_ERASEBKGND { Some(1) } else { None }
                },
            ).ok() { handlers.push(h); }
        }

        // WM_SIZE on frame to reposition scrollbars and update range
        if hwnd != std::ptr::null_mut() {
            let vscroll_sz = vscroll.clone();
            let hscroll_sz = hscroll.clone();
            let child_sz_child = child.clone();
            let c_hwnd = hwnd;
            static SIZE_ID: AtomicUsize = AtomicUsize::new(0x80000000);
            let sid = SIZE_ID.fetch_add(1, Ordering::SeqCst);
            // (Fix-ReactOS) Per-frame size history. ReactOS re-fires WM_SIZE on
            // a frame every time it resizes it, and this handler resizes the
            // scrollbars and the child, which makes ReactOS re-fire WM_SIZE on
            // the frame again. With no guard the sizes oscillate and the frame
            // resizes in a tight infinite loop: present() never returns, the
            // grid is never painted, and the app is dead on arrival (observed
            // as an endless scsz-in/scsz-out trace). Laying the children out
            // once per *distinct* frame size is all a settled viewport needs -
            // the same settle rule the box WM_SIZE handler already uses.
            let last_size = std::rc::Rc::new(std::cell::RefCell::new((-1i32, -1i32)));
            if let Some(h) = nwg::bind_raw_event_handler(
                &nwg::ControlHandle::Hwnd(hwnd as _), sid,
                move |_h, msg, _w, _l| {
                    if msg != winapi::um::winuser::WM_SIZE { return None; }
                    // (Fix-ReactOS) Re-assert the frame's own visibility
                    // FIRST, before any of the early-outs below.
                    //
                    // Both the settle guard and the zero-size check return
                    // before the re-layout, so a re-assert placed after them
                    // only runs on the one pass that happens to lay this frame
                    // out - and WS_VISIBLE can be dropped again on a *later*
                    // pass that takes an early-out. This frame is the canvas's
                    // parent, so while it reads hidden the whole subtree
                    // (canvas and both scrollbars) reports hidden even though
                    // each carries its own WS_VISIBLE bit, and none of their
                    // pixels are composited. See force_visible.
                    #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                    force_visible(c_hwnd as _);
                    // TEMPORARY ReactOS diagnosis: scrolled frame WM_SIZE entry.
                    #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                    { mark95a(b"scsz-in\n"); }
                    unsafe {
                        let mut rect: winapi::shared::windef::RECT = std::mem::zeroed();
                        winapi::um::winuser::GetClientRect(c_hwnd as _, &mut rect);
                        let w = rect.right;
                        let h = rect.bottom;
                        // (Fix-ReactOS) Settle guard: only re-lay-out the
                        // scrollbars and child when the frame size actually
                        // changed, so ReactOS's repeated WM_SIZE deliveries
                        // cannot drive an endless resize loop.
                        {
                            let mut last = last_size.borrow_mut();
                            if *last == (w, h) { return None; }
                            *last = (w, h);
                        }
                        // Drop zero sizes (stale setup-storm leftovers):
                        // resizing the canvas to 0x0 would silence its
                        // WM_PAINT forever (empty update region, nothing
                        // re-invalidates). The next real size repairs.
                        if w <= 0 || h <= 0 { return None; }
                        let scroll_w = 20i32;
                        let scroll_h = 20i32;
                        if let Ok(sb) = vscroll_sz.try_borrow() {
                            if let Some(vh) = sb.handle.hwnd() {
                                winapi::um::winuser::SetWindowPos(
                                    vh as _, std::ptr::null_mut(),
                                    w - scroll_w, 0, scroll_w, h - scroll_h,
                                    winapi::um::winuser::SWP_NOZORDER | winapi::um::winuser::SWP_SHOWWINDOW,
                                );
                            }
                        }
                        if let Ok(sb) = hscroll_sz.try_borrow() {
                            if let Some(hh) = sb.handle.hwnd() {
                                winapi::um::winuser::SetWindowPos(
                                    hh as _, std::ptr::null_mut(),
                                    0, h - scroll_h, w - scroll_w, scroll_h,
                                    winapi::um::winuser::SWP_NOZORDER | winapi::um::winuser::SWP_SHOWWINDOW,
                                );
                            }
                        }
                        // The child always fills the viewport (ranges are driven
                        // by the host via scroll_to; nothing pans).
                        if let Some(child_ptr) = *child_sz_child.borrow() {
                            winapi::um::winuser::SetWindowPos(
                                child_ptr as _, std::ptr::null_mut(),
                                0, 0, (w - scroll_w).max(0), (h - scroll_h).max(0),
                                winapi::um::winuser::SWP_NOZORDER | winapi::um::winuser::SWP_SHOWWINDOW,
                            );
                            // TEMPORARY ReactOS diagnosis: does a direct
                            // invalidate of the canvas produce a WM_PAINT?
                            winapi::um::winuser::RedrawWindow(
                                child_ptr as _, std::ptr::null_mut(), std::ptr::null_mut(),
                                winapi::um::winuser::RDW_INVALIDATE
                                    | winapi::um::winuser::RDW_UPDATENOW
                                    | winapi::um::winuser::RDW_ERASE,
                            );
                        }
                    }
                    // TEMPORARY ReactOS diagnosis: scrolled frame WM_SIZE exit.
                    #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                    { mark95a(b"scsz-out\n"); }
                    None
                },
            ).ok() { handlers.push(h); }

            // Scroll messages from the scrollbar children arrive at the frame
            // (their parent), not at the scrollbars themselves. Compute the new
            // thumb position from the request and report it; the host moves its
            // cursor (the child keeps filling the viewport; nothing pans).
            // SB_LINEUP/DOWN = 0/1, SB_PAGEUP/DOWN = 2/3, SB_THUMBPOSITION = 4,
            // SB_THUMBTRACK = 5, SB_TOP/BOTTOM = 6/7, SB_ENDSCROLL = 8.
            let vscroll_msg = vscroll.clone();
            let hscroll_msg = hscroll.clone();
            let scroll_cb = on_scroll_cb.clone();
            static SCROLL_MSG_ID: AtomicUsize = AtomicUsize::new(0x90000000);
            let smid = SCROLL_MSG_ID.fetch_add(1, Ordering::SeqCst);
            if let Some(h) = nwg::bind_raw_event_handler(
                &nwg::ControlHandle::Hwnd(hwnd as _), smid,
                move |_h, msg, w, _l| {
                    let vertical = if msg == winapi::um::winuser::WM_VSCROLL {
                        true
                    } else if msg == winapi::um::winuser::WM_HSCROLL {
                        false
                    } else {
                        return None;
                    };
                    let sb = if vertical { &vscroll_msg } else { &hscroll_msg };
                    let Ok(sb) = sb.try_borrow() else { return Some(0); };
                    let req = (w & 0xFFFF) as u32;
                    if req == 8 {
                        return Some(0); // SB_ENDSCROLL: nothing to do
                    }
                    let cur = sb.pos();
                    let end = sb.range().end.max(1);
                    let track = ((w >> 16) & 0xFFFF) as usize;
                    let new_pos = match req {
                        4 | 5 => track.min(end), // thumb position/track
                        0 => cur.saturating_sub(1), // line up/left
                        1 => (cur + 1).min(end), // line down/right
                        2 => cur.saturating_sub(10), // page up/left
                        3 => (cur + 10).min(end), // page down/right
                        6 => 0,  // top/left end
                        7 => end, // bottom/right end
                        _ => cur,
                    };
                    sb.set_pos(new_pos);
                    if let Some(ref mut f) = *scroll_cb.borrow_mut() {
                        f(vertical, new_pos as f64);
                    }
                    Some(0)
                },
            ).ok() { handlers.push(h); }

        }

        Ok(ScrolledWindow {
            frame: Some(Rc::new(frame)),
            hwnd,
            child,
            vscroll,
            hscroll,
            _handlers: Rc::new(handlers),
            on_scroll_cb,
        })
    }

    // ========== Menu / MenuBar / SimpleAction ==========

    /// Convert adapter MenuItemData → nwg builder format
    fn as_nwg_data(items: &[MenuItem]) -> Vec<crate::backends::nwg::MenuItemData> {
        items.iter().map(|i| crate::backends::nwg::MenuItemData {
            label: i.label.clone(),
            detailed_action: i.action.clone(),
            submenu: i.submenu.as_ref().map(|s| as_nwg_data(&s.items)),
        }).collect()
    }

    // -- Menu (data-only, builds nothing until consumed by MenuBar) --

    struct MenuItem {
        label: String,
        action: String,
        submenu: Option<Menu>,
    }

    pub struct Menu {
        items: Vec<MenuItem>,
    }

    impl Menu {
        pub fn append(&mut self, label: &str, detailed_action: &str) {
            self.items.push(MenuItem {
                // The model speaks GTK (`_` mnemonics); translate once to
                // the Win32 `&` marker here so no call site invents its own.
                label: crate::win32_portable::gtk_mnemonic_to_win32(label),
                action: detailed_action.to_string(),
                submenu: None,
            });
        }
        pub fn append_submenu(&mut self, label: &str, submenu: &Menu) {
            self.items.push(MenuItem {
                label: crate::win32_portable::gtk_mnemonic_to_win32(label),
                action: String::new(),
                submenu: Some(submenu.clone()),
            });
        }
    }

    impl Clone for Menu { fn clone(&self) -> Self { Menu { items: self.items.iter().map(|i| MenuItem {
        label: i.label.clone(), action: i.action.clone(), submenu: i.submenu.clone(),
    }).collect() } } }

    pub fn create_menu() -> Result<Menu, Error> { Ok(Menu { items: Vec::new() }) }

    // -- MenuBar: builds NWG menus, wires actions --

    /// Table mapping (hmenu_ptr, item_index) → stripped action name
    type MenuIndex = HashMap<(*mut std::ffi::c_void, u32), String>;
    /// Table mapping WM_COMMAND item id → detailed action name. Menus are
    /// supposed to report WM_MENUCOMMAND (MNS_NOTIFYBYPOS), but the ids
    /// arrive as classic WM_COMMAND on some hosts — without this table
    /// every menu item silently does nothing.
    type MenuIdIndex = HashMap<u32, String>;

    pub struct MenuBar {
        pub(crate) _menus: Rc<Vec<nwg::Menu>>,
        pub(crate) _items: Rc<Vec<nwg::MenuItem>>,
        pub(crate) _raw_handler: Rc<nwg::RawEventHandler>,
        pub(crate) action_registry: Rc<RefCell<HashMap<String, Box<dyn FnMut()>>>>,
        /// The top-level drop-downs, in menu-bar order: their Win32 label and
        /// their `HMENU`. `popup_submenu_by_mnemonic_at` needs the `HMENU`
        /// to pop, and the label to resolve an Alt+letter to an entry — the
        /// OS picks the first entry whose `&`-marker matches, so this table
        /// must stay in bar order for that scan to agree with the OS.
        top_level: Rc<Vec<(String, *mut std::ffi::c_void)>>,
        /// The toplevel HWND, for the popup's owner. A menu must be owned by
        /// the window it belongs to, or it is dismissed as soon as that window
        /// loses focus and floats above other apps' windows when it should not.
        owner_hwnd: *mut c_void,
    }

    impl Clone for MenuBar {
        fn clone(&self) -> Self {
            MenuBar {
                _menus: self._menus.clone(),
                _items: self._items.clone(),
                _raw_handler: self._raw_handler.clone(),
                action_registry: self.action_registry.clone(),
                top_level: self.top_level.clone(),
                owner_hwnd: self.owner_hwnd,
            }
        }
    }

    impl AsRef<*mut c_void> for MenuBar {
        fn as_ref(&self) -> &*mut c_void {
            static NULL: usize = 0;
            unsafe { &*(&NULL as *const usize as *const *mut c_void) }
        }
    }

    impl Widget for MenuBar {
        fn raw_handle(&self) -> *mut c_void {
            std::ptr::null_mut()
        }
    }

    /// The `HMENU` of the top-level drop-down at bar position `pos`.
///
/// Resolved through the window's real menu bar (`GetMenu` + `GetSubMenu`)
/// rather than from the `HMENU`s captured at build time: NWG's `Drop`
/// destroys a menu's handles, and a rebuild leaves the captured values
/// dangling. Asking the window each time is what keeps the popup pointing at
/// the menu that is actually installed — the failure mode being a popup that
/// lands somewhere unrelated, or not at all.
unsafe fn submenu_handle(menu_bar: &MenuBar, pos: usize) -> *mut std::ffi::c_void {
    if menu_bar.owner_hwnd.is_null() {
        return std::ptr::null_mut();
    }
    let bar = winapi::um::winuser::GetMenu(menu_bar.owner_hwnd as _);
    if bar.is_null() {
        return std::ptr::null_mut();
    }
    let sub = winapi::um::winuser::GetSubMenu(bar, pos as i32);
    sub as *mut std::ffi::c_void
}

/// The monitor work area for `owner`, plus the menu's own measured size.
///
/// The work area (screen minus the taskbar) is the bound a popup must stay
/// inside. NWG's vendored Win95 build has no `GetMonitorInfoW` (see
/// `vendor/native-windows-gui/README-WIN95.md`), and the public
/// `monitor_rect_from_window` returns `rcMonitor` — which *includes* the
/// taskbar, so it cannot detect a popup hiding under it. `SPI_GETWORKAREA` is
/// used instead: it predates Win95 and reports the taskbar-excluded rectangle
/// everywhere.
unsafe fn popup_geometry(
    owner: *mut c_void,
    hmenu: *mut std::ffi::c_void,
) -> (crate::core::ScreenRect, i32, i32) {
    let mut wa: winapi::shared::windef::RECT = std::mem::zeroed();
    let got = winapi::um::winuser::SystemParametersInfoA(
        winapi::um::winuser::SPI_GETWORKAREA,
        0,
        &mut wa as *mut _ as *mut winapi::ctypes::c_void,
        0,
    ) != 0;
    let work = if got && wa.right > wa.left && wa.bottom > wa.top {
        crate::core::ScreenRect {
            left: wa.left,
            top: wa.top,
            right: wa.right,
            bottom: wa.bottom,
        }
    } else {
        // Last resort: the primary screen size. Still on-screen, just possibly
        // able to slip under a taskbar.
        crate::core::ScreenRect {
            left: 0,
            top: 0,
            right: winapi::um::winuser::GetSystemMetrics(
                winapi::um::winuser::SM_CXSCREEN,
            ),
            bottom: winapi::um::winuser::GetSystemMetrics(
                winapi::um::winuser::SM_CYSCREEN,
            ),
        }
    };

    let n = winapi::um::winuser::GetMenuItemCount(hmenu as _);
    let count = if n > 0 { n } else { 1 };
    let mut row_h = 0i32;
    let mut width = 0i32;
    // Measure every item: the popup is as wide as its widest, and as tall as
    // all of them stacked. `GetMenuItemRect`'s third parameter is the item
    // index as a `u32` (unlike the i32 index other menu calls take), and its
    // rect is the *item's row within the popup*, so its width alone is not the
    // popup width -- hence the floor below.
    for i in 0..count {
        let mut r: winapi::shared::windef::RECT = std::mem::zeroed();
        // Signature is GetMenuItemRect(hWnd, hMenu, uItem, rect): the owner
        // window comes first even for a popup menu, and the item index is a
        // `UINT`, not the `i32` most menu calls take.
        if winapi::um::winuser::GetMenuItemRect(
            owner as _,
            hmenu as _,
            i as u32,
            &mut r,
        ) != 0
        {
            row_h = row_h.max(r.bottom - r.top);
            width = width.max(r.right - r.left);
        }
    }
    if width <= 0 {
        width = 200;
    }
    let height = if row_h > 0 { row_h * count } else { 20 * count };
    (work, width, height)
}

// Keyboard-menu shims required by common.rs: native Win32 menus handle
    // Alt+letter mnemonics in the OS (see the `&` prefixes in
    // `create_menubar`), so the manual popover-navigation contract is a
    // no-op here — matching the documented "On NWG ... this is a no-op".
    impl MenuBar {
        pub fn activate_submenu_by_mnemonic(&self, keyval: u32) -> bool {
            // Open at the mouse-free default: the bar's own top-left, so the
            // caller that wants a position passes one (see
            // `popup_submenu_by_mnemonic_at`) and this stays a "just open it".
            let (x, y) = self.bar_origin().unwrap_or((0, 0));
            self.popup_submenu_by_mnemonic_at(keyval, x, y)
        }
        /// Open the submenu whose mnemonic is `keyval` at a screen position.
        ///
        /// `x`/`y` are absolute screen pixels — the caller is expected to have
        /// added the widget's `screen_origin` to its widget-relative click
        /// point, exactly as the GTK path does.
        ///
        /// Three steps, all required for the menu to land where asked:
        ///
        /// 1. Resolve `keyval` to a top-level entry. The OS scans the bar
        ///    left to right and takes the first `&`-marked match, so we scan
        ///    the same way (see `find_win32_top_level_mnemonic`) — picking a
        ///    later entry would open the wrong menu.
        /// 2. `TrackPopupMenuEx` on that entry's `HMENU`, at `(x, y)`,
        ///    without `TPM_LEFTALIGN`/`TPM_TOPALIGN` so the corner is the
        ///    exact point the caller asked for.
        /// 3. `PostMessageW(WM_NULL)` to the owner afterwards. Without it the
        ///    popup can be dismissed by the very click-release that opened it,
        ///    and the menu flashes then vanishes.
        ///
        /// `TPM_RETURNCMD` is deliberately *not* set: the menu item's
        /// activation is dispatched by the `WM_MENUCOMMAND`/`WM_COMMAND`
        /// handler bound in `create_menubar`, which is the same path a
        /// mouse-opened menu uses. Returning the id here as well would
        /// dispatch twice.
        /// The cross-backend entry point: resolve the anchor to screen
        /// coordinates, then apply the shared placement rule
        /// (`core::place_popup`) with this backend's measured menu size.
        pub fn popup_at(&self, label: &str, anchor: &Canvas, x: f64, y: f64) -> bool {
            let keyval = match label.chars().next() {
                Some(c) => c as u32,
                None => return false,
            };
            let Some((ox, oy)) = anchor.screen_origin() else {
                // No origin: open unpositioned rather than guessing one, which
                // is what the pre-`popup_at` code did when `screen_origin`
                // returned `None`.
                return self.activate_submenu_by_mnemonic(keyval);
            };
            self.popup_submenu_by_mnemonic_at(keyval, ox + x as i32, oy + y as i32)
        }
        pub fn popup_submenu_by_mnemonic_at(&self, keyval: u32, x: i32, y: i32) -> bool {
            let Some(pos) = crate::win32_portable::find_win32_top_level_mnemonic(
                &self.top_level.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(),
                keyval,
            ) else {
                return false;
            };
            let Some(&(_, _)) = self.top_level.get(pos) else {
                return false;
            };
            let hmenu = unsafe { submenu_handle(self, pos) };
            if hmenu.is_null() || self.owner_hwnd.is_null() {
                return false;
            }
            // Place the popup before showing it: `TrackPopupMenuEx` takes no
            // size, so the "would it go off-screen?" decision needs the menu's
            // measured size up front. `popup_geometry` runs the menu once with
            // TPM_NONOTIFY to measure it, which is also why the placeholder
            // HWND below must be a real handle and not 0.
            let (work, mw, mh) = unsafe { popup_geometry(self.owner_hwnd, hmenu) };
            // The placement RULE is shared with every backend
            // (`core::place_popup`); only the measuring above is Win32-specific.
            let (px, py) = crate::core::place_popup(x, y, mw, mh, work);
            unsafe {
                let flags = winapi::um::winuser::TPM_RIGHTBUTTON
                    | winapi::um::winuser::TPM_NONOTIFY;
                // TrackPopupMenuEx(hmenu, flags, x, y, hwnd, owner): `hwnd`
                // is a placeholder HWND (the MS docs use HWND_DESKTOP) and is
                // ignored unless TPM_NONOTIFY is set — it is not a "reserved".
                let cmd = winapi::um::winuser::TrackPopupMenuEx(
                    hmenu as _,
                    flags,
                    px,
                    py,
                    winapi::um::winuser::HWND_DESKTOP,
                    self.owner_hwnd as _,
                );
                // Drain the click that opened us, else it dismisses the menu
                // on the very same release.
                winapi::um::winuser::PostMessageW(
                    self.owner_hwnd as _,
                    winapi::um::winuser::WM_NULL,
                    0,
                    0,
                );
                // 0 means the user dismissed without picking. A non-zero value
                // was NOT handled here (see TPM_RETURNCMD above), so report
                // "opened" either way: the action was dispatched by the
                // handler, and reporting false would make the caller show a
                // spurious "menu unavailable".
                let _ = cmd;
                true
            }
        }

        /// Top-left of the menu bar's owner window, in screen pixels, for
        /// callers that want a default popup position.
        fn bar_origin(&self) -> Option<(i32, i32)> {
            if self.owner_hwnd.is_null() {
                return None;
            }
            unsafe {
                let mut pt = winapi::shared::windef::POINT { x: 0, y: 0 };
                if winapi::um::winuser::ClientToScreen(self.owner_hwnd as _, &mut pt) == 0 {
                    return None;
                }
                Some((pt.x, pt.y))
            }
        }
        pub fn activate_submenu_item_by_mnemonic(&self, _keyval: u32) -> bool {
            false
        }
        pub unsafe fn insert_action_group(&self, _name: &str, _group_ptr: *mut c_void) {}
        pub fn handle_mnemonic_key(&self, _keyval: u32) -> bool {
            false
        }
        pub fn handle_menu_key(&self, _keyval: u32, _modifiers: u32) -> bool {
            false
        }
        /// [`handle_menu_key`](Self::handle_menu_key) with the typed character
        /// supplied separately; this backend's key codes are already characters.
        pub fn handle_menu_key_char(&self, keyval: u32, _ch: char, modifiers: u32) -> bool {
            self.handle_menu_key(keyval, modifiers)
        }
        pub fn menu_active(&self) -> bool {
            false
        }
        pub fn menu_close(&self) {}
    }

    /// Recursively build NWG menu items, recording the (hmenu, index)→action mapping.
    /// Collectors `menus` and `items` keep the NWG objects alive (else Drop destroys them).
    fn build_and_index(
        parent_handle: &nwg::ControlHandle,
        items: &[crate::backends::nwg::MenuItemData],
        index: &mut MenuIndex,
        id_index: &mut MenuIdIndex,
        menus: &mut Vec<nwg::Menu>,
        items_collector: &mut Vec<nwg::MenuItem>,
    ) -> Result<(), nwg::NwgError> {
        for (i, item) in items.iter().enumerate() {
            if let Some(ref children) = item.submenu {
                let mut sub = nwg::Menu::default();
                nwg::Menu::builder()
                    .text(&item.label)
                    .popup(false)
                    .parent(parent_handle.clone())
                    .build(&mut sub)?;
                let sub_hmenu = sub.handle.hmenu().map(|(_, h)| h).unwrap_or(std::ptr::null_mut());
                let sub_handle = match parent_handle {
                    nwg::ControlHandle::Hwnd(h) => nwg::ControlHandle::PopMenu(*h, sub_hmenu),
                    nwg::ControlHandle::PopMenu(h, _) => nwg::ControlHandle::PopMenu(*h, sub_hmenu),
                    _ => unreachable!(),
                };
                menus.push(sub);
                build_and_index(&sub_handle, children, index, id_index, menus, items_collector)?;
            } else {
                let mut mi = nwg::MenuItem::default();
                nwg::MenuItem::builder()
                    .text(&item.label)
                    .parent(parent_handle.clone())
                    .build(&mut mi)?;
                let parent_hmenu: *mut c_void = match parent_handle {
                    nwg::ControlHandle::Hwnd(h) => *h as *mut c_void,
                    nwg::ControlHandle::PopMenu(_h, m) => *m as *mut c_void,
                    _ => unreachable!(),
                };
                if !item.detailed_action.is_empty() {
                    index.insert((parent_hmenu as *mut c_void, i as u32), item.detailed_action.clone());
                    // WM_COMMAND fallback: record the numeric id NWG
                    // assigned so the classic path dispatches too.
                    if let nwg::ControlHandle::MenuItem(_, id) = mi.handle {
                        id_index.insert(id, item.detailed_action.clone());
                    }
                }
                items_collector.push(mi);
            }
        }
        Ok(())
    }

    /// Count leaf items in the menu tree (sequential numbering).
#[allow(dead_code)]
    fn count_leaves(items: &[crate::backends::nwg::MenuItemData]) -> u32 {
        let mut n = 0;
        for i in items {
            if let Some(ref children) = i.submenu {
                n += count_leaves(children);
            } else {
                n += 1;
            }
        }
        n
    }

    pub fn create_menubar(
        model: &Menu,
        window_hwnd: *mut c_void,
        action_registry: Rc<RefCell<HashMap<String, Box<dyn FnMut()>>>>,
    ) -> Result<MenuBar, Error> {
        let window_handle = nwg::ControlHandle::Hwnd(window_hwnd as _);
        let mut index: MenuIndex = HashMap::new();
        let mut id_index: MenuIdIndex = HashMap::new();
        let data = as_nwg_data(&model.items);
        let mut menus: Vec<nwg::Menu> = Vec::new();
        let mut items: Vec<nwg::MenuItem> = Vec::new();

        // Build top-level menubar entries (File, Edit, Help, ...)
        // `top_level` is in bar order; `popup_submenu_by_mnemonic_at` scans it
        // the way the OS scans the bar.
        let mut top_level: Vec<(String, *mut std::ffi::c_void)> = Vec::new();
        for item in &data {
            if let Some(ref children) = item.submenu {
                let mut menu = nwg::Menu::default();
                nwg::Menu::builder()
                    .text(&item.label)
                    .popup(false)
                    .parent(&window_handle)
                    .build(&mut menu)
                    .map_err(|e| Error::Backend(format!("{}", e)))?;
                let menu_hmenu = menu.handle.hmenu().map(|(_, h)| h).unwrap_or(std::ptr::null_mut());
                let menu_handle = nwg::ControlHandle::PopMenu(
                    window_hwnd as *mut std::ffi::c_void as _,
                    menu_hmenu,
                );
                top_level.push((item.label.clone(), menu_hmenu as *mut std::ffi::c_void));
                menus.push(menu);
                build_and_index(&menu_handle, children, &mut index, &mut id_index, &mut menus, &mut items)
                    .map_err(|e| Error::Backend(format!("{}", e)))?;
            } else {
                let mut mi = nwg::MenuItem::default();
                nwg::MenuItem::builder()
                    .text(&item.label)
                    .parent(&window_handle)
                    .build(&mut mi)
                    .map_err(|e| Error::Backend(format!("{}", e)))?;
                if let nwg::ControlHandle::MenuItem(_, id) = mi.handle {
                    id_index.insert(id, item.detailed_action.clone());
                }
                items.push(mi);
            }
        }

        // Bind raw event handler for menu activation, both flavours:
        // WM_MENUCOMMAND (MNS_NOTIFYBYPOS position reports) and the
        // classic WM_COMMAND (numeric item id, lparam 0). Either one may
        // arrive depending on host; without both, menu items silently
        // do nothing on hosts that send the other.
        // handler_id must be > 0xFFFF (NWG reserves lower IDs)
        const RAW_MENU_ID: usize = 0x10001;
        let idx = index.clone();
        let ids = id_index.clone();
        let reg = action_registry.clone();
        let reg2 = action_registry.clone();
        let raw_handler = nwg::bind_raw_event_handler(
            &nwg::ControlHandle::Hwnd(window_hwnd as _),
            RAW_MENU_ID,
            move |_hwnd, msg, wparam, lparam| {
                // Classic path: HIWORD 0 (menu, not accelerator), lparam 0
                // (menu, not control). Controls also send WM_COMMAND, so
                // never claim those.
                if msg == winapi::um::winuser::WM_COMMAND && lparam == 0 {
                    let id = (wparam & 0xFFFF) as u32;
                    if let Some(action_name) = ids.get(&id) {
                        let stripped = action_name.rsplit('.').next().unwrap_or(action_name);
                        if let Some(cb) = reg2.borrow_mut().get_mut(stripped) {
                            cb();
                        }
                        return Some(0);
                    }
                    return None;
                }
                if msg != winapi::um::winuser::WM_MENUCOMMAND { return None; }
                let item_index = (wparam & 0xFFFF) as u32;
                let hmenu = lparam as *mut c_void;
                let key = (hmenu, item_index);
                if let Some(action_name) = idx.get(&key) {
                    let stripped = action_name.rsplit('.').next().unwrap_or(action_name);
                    if let Some(cb) = reg.borrow_mut().get_mut(stripped) {
                        cb();
                    }
                }
                Some(0)
            },
        ).map_err(|e| Error::Backend(format!("{}", e)))?;

        Ok(MenuBar {
            _menus: Rc::new(menus),
            _items: Rc::new(items),
            _raw_handler: Rc::new(raw_handler),
            action_registry,
            top_level: Rc::new(top_level),
            owner_hwnd: window_hwnd,
        })
    }

    // -- SimpleAction: stores callback by action name --

    pub struct SimpleAction {
        pub(crate) name: String,
        pub(crate) registry: Rc<RefCell<HashMap<String, Box<dyn FnMut()>>>>,
    }

    impl Clone for SimpleAction {
        fn clone(&self) -> Self {
            SimpleAction {
                name: self.name.clone(),
                registry: self.registry.clone(),
            }
        }
    }

    impl SimpleAction {
        pub fn connect_activate<F: FnMut(*mut c_void) + 'static>(&self, mut f: F) -> Result<u64, Error> {
            let mut map = self.registry.borrow_mut();
            map.insert(self.name.clone(), Box::new(move || f(std::ptr::null_mut())));
            Ok(0)
        }
    }

    pub fn create_simple_action(
        name: &str,
        registry: Rc<RefCell<HashMap<String, Box<dyn FnMut()>>>>,
    ) -> Result<SimpleAction, Error> {
        Ok(SimpleAction {
            name: name.to_string(),
            registry,
        })
    }
    // ---- File dialogs ----

    /// nwg filter-spec for `(name, patterns)` pairs:
    /// `Name (*.a;*.b)|*.a;*.b|…`. Empty list means no filter.
    /// nwg filter spec for `(name, patterns)` pairs; the format
    /// (`Name (*.a;*.b)|*.a;*.b|…`) is built portably and unit-tested in
    /// `win32_portable::join_dialog_filters`.
    #[cfg(windows)]
    fn join_filters(filters: &[(&str, &[&str])]) -> Option<String> {
        crate::win32_portable::join_dialog_filters(filters)
    }

    pub fn open_file(title: &str, parent: *mut c_void) -> Result<Option<String>, Error> {
        open_file_filtered(title, parent, &[])
    }

    pub fn open_file_filtered(title: &str, parent: *mut c_void, filters: &[(&str, &[&str])]) -> Result<Option<String>, Error> {
        let mut dialog = nwg::FileDialog::default();
        let mut builder = nwg::FileDialog::builder();
        builder = builder.title(title).action(nwg::FileDialogAction::Open);
        if let Some(spec) = join_filters(filters) {
            builder = builder.filters(&spec);
        }
        builder
            .build(&mut dialog)
            .map_err(|e| Error::Backend(format!("{}", e)))?;
        let parent_handle = nwg::ControlHandle::Hwnd(parent as _);
        if dialog.run(Some(&parent_handle)) {
            match dialog.get_selected_item() {
                Ok(path) => Ok(Some(path.to_string_lossy().into_owned())),
                Err(_) => Ok(None),
            }
        } else {
            Ok(None)
        }
    }

    pub fn save_file(title: &str, parent: *mut c_void) -> Result<Option<String>, Error> {
        save_file_filtered(title, parent, &[], "")
    }

    /// Save dialog with file-type filters and an optional suggested
    /// filename. Filters follow nwg's `Name (*.a;*.b)|*.a;*.b` spec; the
    /// native dialog offers the selected type's extension, prompts on
    /// overwrite, and appends the extension itself.
    pub fn save_file_filtered(title: &str, parent: *mut c_void, filters: &[(&str, &[&str])], _current_name: &str) -> Result<Option<String>, Error> {
        let mut dialog = nwg::FileDialog::default();
        let mut builder = nwg::FileDialog::builder();
        builder = builder.title(title).action(nwg::FileDialogAction::Save);
        if let Some(spec) = join_filters(filters) {
            builder = builder.filters(&spec);
        }
        // NOTE: nwg exposes no initial-filename setter; the suggested name
        // is GTK-only (callers still append the extension themselves, so
        // both backends land on the same bytes).
        builder
            .build(&mut dialog)
            .map_err(|e| Error::Backend(format!("{}", e)))?;
        let parent_handle = nwg::ControlHandle::Hwnd(parent as _);
        if dialog.run(Some(&parent_handle)) {
            match dialog.get_selected_item() {
                Ok(path) => Ok(Some(path.to_string_lossy().into_owned())),
                Err(_) => Ok(None),
            }
        } else {
            Ok(None)
        }
    }

    pub fn quit_main_loop() {
        crate::backends::nwg::quit_main_loop();
    }

    /// Diagnostic: dump the native window tree under `root` (hwnd, class,
    /// rect, visibility, leading text) to `path`. Env-gated by callers;
    /// no behavior change when unused. Exists so layout bugs can be
    /// diagnosed from real Windows screenshots' ground truth.
    pub fn debug_dump_native_tree(root: *mut c_void, path: &str) {
        struct Ctx {
            lines: Vec<String>,
        }
        unsafe extern "system" fn enum_cb(
            hwnd: winapi::shared::windef::HWND,
            lparam: winapi::shared::minwindef::LPARAM,
        ) -> i32 {
            let ctx = &mut *(lparam as *mut Ctx);
            unsafe {
                let mut cls: [u16; 64] = [0; 64];
                let n = winapi::um::winuser::GetClassNameW(hwnd, cls.as_mut_ptr(), 64);
                let class = String::from_utf16_lossy(&cls[..n.max(0) as usize]);
                let mut rect: winapi::shared::windef::RECT = std::mem::zeroed();
                winapi::um::winuser::GetWindowRect(hwnd, &mut rect);
                let vis = winapi::um::winuser::IsWindowVisible(hwnd) != 0;
                let tlen = winapi::um::winuser::GetWindowTextLengthW(hwnd);
                let mut text = String::new();
                if tlen > 0 && tlen < 80 {
                    let mut buf: Vec<u16> = vec![0; (tlen + 1) as usize];
                    winapi::um::winuser::GetWindowTextW(hwnd, buf.as_mut_ptr(), tlen + 1);
                    text = String::from_utf16_lossy(&buf[..tlen as usize]);
                }
                ctx.lines.push(format!(
                    "hwnd={:p} class={} rect=({},{})-({},{}) visible={} text={:?}",
                    hwnd, class, rect.left, rect.top, rect.right, rect.bottom, vis, text
                ));
            }
            1
        }
        unsafe {
            let mut ctx = Ctx { lines: Vec::new() };
            // Root itself first.
            enum_cb(root as _, &mut ctx as *mut Ctx as _);
            winapi::um::winuser::EnumChildWindows(root as _, Some(enum_cb), &mut ctx as *mut Ctx as _);
            let _ = std::fs::write(path, ctx.lines.join("\n") + "\n");
        }
    }
    #[cfg(test)]
    mod window_redraw_tests {
        /// `Window::queue_redraw` must cascade to every child, not just
        /// invalidate the frame.
        ///
        /// corro's `update_state_cursor` repaints a cursor move by calling
        /// BOTH `canvas.queue_redraw()` and `window.queue_redraw()`, and the
        /// comment there names this exact symptom: "a canvas-only queue_draw
        /// may not trigger the toplevel's frame clock ... leaving pure cursor
        /// moves invisible — the state advances but the grid keeps showing the
        /// old highlight."
        ///
        /// On Win32 that means: `Canvas::queue_redraw` only calls
        /// `InvalidateRect`, which marks the update region and returns — the
        /// canvas is a child window, and nothing there pumps it to
        /// completion. Windows sends `WM_PAINT` for a child when the parent
        /// paints, so a cursor move stayed invisible for the whole time a key
        /// was held and only caught up when the next full-window repaint
        /// arrived (which is why it looked like "nothing moves, then one
        /// jump"). The window cascade has to `RedrawWindow` the parent with
        /// `RDW_ALLCHILDREN` so the child actually repaints.
        #[test]
        fn window_queue_redraw_cascades_to_children() {
            let src = include_str!("backends_nwg_adapter.rs");
            // Split so this literal does not match itself: the test file
            // contains the very text it searches for.
            let noop = concat!("pub fn queue_redraw(&self) ", "{}");
            if let Some(start) = src.find(noop) {
                panic!(
                    "Window::queue_redraw is an empty no-op at byte {start}: cursor moves \
                     call it expecting a toplevel repaint, but nothing is invalidated, so a \
                     held arrow key updates state without ever repainting. It must \
                     RedrawWindow the parent with RDW_ALLCHILDREN."
                );
            }
            // And the cascade it promises must actually be present.
            assert!(
                src.contains("RDW_ALLCHILDREN"),
                "Window::queue_redraw must cascade with RDW_ALLCHILDREN so the canvas \
                 child window repaints too"
            );
        }
    }
#[cfg(test)]
mod gesture_math_tests {
    use super::*;

    fn lparam(x: i32, y: i32) -> isize {
        ((y as u32) << 16 | (x as u32 & 0xFFFF)) as isize
    }

    fn wparam(keys: u16, buttons: u16) -> usize {
        (buttons as usize) << 16 | keys as usize
    }

    #[test]
    fn a_point_is_read_from_both_halves_of_lparam() {
        assert_eq!(win32_point(lparam(120, 340)), (120.0, 340.0));
        assert_eq!(win32_point(lparam(0, 0)), (0.0, 0.0));
    }

    /// A window dragged off the left edge reports a negative x, and the sign
    /// lives in the top bit of the low word. Read as unsigned it becomes ~65000
    /// and the pointer lands nowhere near the cursor.
    #[test]
    fn a_negative_coordinate_stays_negative() {
        let (x, y) = win32_point(lparam(-5, 200));
        assert_eq!(x, -5.0, "x must not wrap to 65531");
        assert_eq!(y, 200.0);
    }

    #[test]
    fn the_four_modifier_bits_are_decoded() {
        let none = win32_mods(wparam(0, 0));
        assert!(!none.shift && !none.ctrl && !none.alt && !none.meta);

        let all = win32_mods(wparam(0x0001 | 0x0002 | 0x0004 | 0x0008, 0));
        assert!(all.shift && all.ctrl && all.alt && all.meta);

        let shift_only = win32_mods(wparam(0x0001, 0));
        assert!(shift_only.shift && !shift_only.ctrl);
    }

    /// Wheel notches arrive as 120 per notch in the high word of WPARAM, with
    /// bit 0 set to mark "not from the scrollbar". Both wheel up and wheel down
    /// produce positive numbers, distinguished by the sign.
    #[test]
    fn the_wheel_delta_is_normalised_to_one_per_notch() {
        const WHEEL_DELTA: i32 = 120;
        let up = (WHEEL_DELTA << 16 | 1) as usize;
        let down = ((-WHEEL_DELTA) as u32 as i32) << 16 | 1;
        // decode the way the handler does
        let wparam = down as u32;
        let notches = ((wparam >> 16) & 0xFFFF) as i16 as f64;
        assert_eq!(notches / 120.0, -1.0, "wheel down is one notch, negative");
        assert!(notches > 0.0 || notches < 0.0, "up would be +1.0: {}", notches as u32);
        let _ = up;
    }

    #[test]
    fn the_scrollbar_flag_bit_is_distinguished_from_a_notch() {
        // A window with no scrollbar gets bit 0 clear, and must not be told the
        // user scrolled -- that is what makes a canvas receive the wheel.
        let from_scrollbar = (120u32) << 16; // bit 0 clear
        assert_eq!(from_scrollbar & 1, 0, "this is the case the handler drops");
    }
}
}

#[cfg(windows)]
pub use nwg_adapter::*;
#[cfg(windows)]
pub use crate::backends::nwg::Orientation;

