// Win95/rust9x custom target_family gates are intentional
#![cfg_attr(windows, allow(unexpected_cfgs))]

#[cfg(windows)]
mod nwg_backend {
    use native_windows_gui as nwg;
    use std::cell::RefCell;
    use std::os::raw::c_void;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    pub struct NwgApp {
        #[allow(dead_code)]
        pub(crate) current_parent: Rc<RefCell<Option<*mut c_void>>>,
    }

    // TEMPORARY Win95 diagnosis: raw file marker (std::fs broken on 9x).
    #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
    unsafe fn mark95nwg(s: &[u8]) {
        use std::os::raw::c_void as CV;
        unsafe extern "system" {
            fn CreateFileA(name: *const u8, access: u32, share: u32, sa: *mut CV,
                disp: u32, flags: u32, tmpl: *mut CV) -> *mut CV;
            fn SetFilePointer(h: *mut CV, lo: i32, hi: *mut i32, how: u32) -> u32;
            fn WriteFile(h: *mut CV, buf: *const u8, len: u32, w: *mut u32, ov: *mut CV) -> i32;
            fn CloseHandle(h: *mut CV) -> i32;
        }
        let h = CreateFileA(b"c:\\gcorro.log\0".as_ptr(), 0x4000_0000, 1,
            std::ptr::null_mut(), 4, 0x80, std::ptr::null_mut());
        if h.is_null() || h as isize == -1 {
            return;
        }
        SetFilePointer(h, 0, std::ptr::null_mut(), 2);
        let mut w = 0u32;
        WriteFile(h, s.as_ptr(), s.len() as u32, &mut w, std::ptr::null_mut());
        CloseHandle(h);
    }

    impl NwgApp {
        pub fn new() -> Result<Self, nwg::NwgError> {
            nwg::init()?;
            Ok(NwgApp {
                current_parent: Rc::new(RefCell::new(None)),
            })
        }
    }

    pub fn create_hidden_parent() -> Result<*mut c_void, crate::Error> {
        let mut hidden = nwg::Window::default();
        nwg::Window::builder()
            .flags(nwg::WindowFlags::WINDOW)
            .size((1, 1))
            .build(&mut hidden).map_err(|e| crate::Error::Backend(e.to_string()))?;
        let hwnd = hidden.handle.hwnd().unwrap_or(std::ptr::null_mut());
        // Leak the window so it stays alive for the app's lifetime
        std::mem::forget(hidden);
        Ok(hwnd as *mut c_void)
    }

    static QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);

    pub fn quit_main_loop() {
        QUIT_REQUESTED.store(true, Ordering::SeqCst);
        unsafe {
            winapi::um::winuser::PostQuitMessage(0);
        }
    }

    pub fn is_quit_requested() -> bool {
        QUIT_REQUESTED.load(Ordering::SeqCst)
    }

    impl crate::backends::BackendApp for NwgApp {
        fn run(self: Box<Self>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            // TEMPORARY Win95 diagnosis.
            #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
            unsafe { mark95nwg(b"nwg-run\n"); }
            // TEMPORARY Win95 diagnosis: is anything queued before we block?
            // If the queue is empty yet GetMessageW returns 0 (WM_QUIT), the
            // call itself misbehaves on Win95 and we must poll instead.
            #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
            unsafe {
                use winapi::um::winuser::{PeekMessageW, MSG, PM_NOREMOVE};
                use std::mem;
                let mut peek: MSG = mem::zeroed();
                if PeekMessageW(&mut peek, std::ptr::null_mut(), 0, 0, PM_NOREMOVE) != 0 {
                    // Log low byte of the message id as two hex chars.
                    let m = peek.message as u32;
                    let hx = b"0123456789abcdef";
                    let mut s = *b"peek-....\n";
                    s[5] = hx[((m >> 12) & 0xf) as usize];
                    s[6] = hx[((m >> 8) & 0xf) as usize];
                    s[7] = hx[((m >> 4) & 0xf) as usize];
                    s[8] = hx[(m & 0xf) as usize];
                    mark95nwg(&s);
                } else {
                    mark95nwg(b"peek-empty\n");
                }
            }
            // Reset quit flag so a re-run works
            QUIT_REQUESTED.store(false, Ordering::SeqCst);
            // Custom message loop: same as nwg::dispatch_thread_events() but
            // without IsDialogMessageW, so Enter/Escape keys reach the Edit
            // control's raw event handler instead of being consumed.
            unsafe {
                use winapi::um::winuser::{GetMessageW, TranslateMessage, DispatchMessageW, MSG, PM_REMOVE, PeekMessageW};
                // TEMPORARY Win95 diagnosis: blocking GetMessageW returns 0
                // (WM_QUIT) immediately on an empty queue, so poll instead.
                #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                use winapi::um::winuser::{MsgWaitForMultipleObjects, QS_ALLINPUT, WM_QUIT};
                use std::mem;
                let mut msg: MSG = mem::zeroed();
                #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                loop {
                    if PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                        if msg.message == WM_QUIT {
                            mark95nwg(b"poll-quit\n");
                            break;
                        }
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    } else {
                        MsgWaitForMultipleObjects(0, std::ptr::null_mut(), 0, 50, QS_ALLINPUT);
                    }
                    if QUIT_REQUESTED.load(Ordering::SeqCst) {
                        mark95nwg(b"poll-flag\n");
                        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                            TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                        break;
                    }
                }
                #[cfg(not(all(target_family = "rust9x", target_env = "msvc")))]
                loop {
                    let ret = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
                    if ret == 0 {
                        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                        unsafe { mark95nwg(b"msg-quit\n"); }
                        break; // WM_QUIT received
                    }
                    if ret == -1 {
                        // Error — exit loop
                        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
                        unsafe { mark95nwg(b"msg-err\n"); }
                        break;
                    }
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                    // Fallback: check quit flag in case PostQuitMessage was
                    // not called (e.g., raw event handler consumed WM_CLOSE
                    // without calling DefWindowProc, or quit was requested
                    // from a non-message context).
                    if QUIT_REQUESTED.load(Ordering::SeqCst) {
                        // Drain remaining messages without blocking so that
                        // any pending cleanup (WM_DESTROY, etc.) runs before
                        // we exit.
                        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                            TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                        break;
                    }
                }
            }
            Ok(())
        }
    }

    pub fn init() -> Result<Box<dyn crate::backends::BackendApp>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Box::new(NwgApp::new().map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?))
    }

    // -- Window --

    pub fn create_window(parent_cell: &Rc<RefCell<Option<*mut c_void>>>) -> Result<(nwg::Window, nwg::EventHandler), nwg::NwgError> {
        let mut win = nwg::Window::default();
        let b = nwg::Window::builder()
            .flags(nwg::WindowFlags::MAIN_WINDOW | nwg::WindowFlags::VISIBLE);
        // TEMPORARY Win95 diagnosis: fit the 640x480 VM screen.
        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
        let b = b.size((620, 400)).position((5, 5));
        #[cfg(not(all(target_family = "rust9x", target_env = "msvc")))]
        let b = b.size((700, 400));
        b.build(&mut win)?;
        let hwnd = win.handle.hwnd().unwrap_or(std::ptr::null_mut());
        *parent_cell.borrow_mut() = Some(hwnd as *mut c_void);
        let handler = nwg::bind_event_handler(
            &nwg::ControlHandle::Hwnd(hwnd),
            &nwg::ControlHandle::Hwnd(hwnd),
            |_evt, _data, _handle| {},
        );
        Ok((win, handler))
    }

    // -- Control creation --

    pub fn create_button(
        parent: *mut c_void,
        text: &str,
    ) -> Result<(nwg::Button, Rc<RefCell<Option<Box<dyn FnMut()>>>>, nwg::EventHandler), nwg::NwgError> {
        let mut btn = nwg::Button::default();
        nwg::Button::builder()
            .text(text)
            .flags(nwg::ButtonFlags::VISIBLE | nwg::ButtonFlags::TAB_STOP)
            .parent(&nwg::ControlHandle::Hwnd(parent as _))
            .build(&mut btn)?;
        let click_cb: Rc<RefCell<Option<Box<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
        let cb = click_cb.clone();
        let hwnd = btn.handle.hwnd().unwrap();
        let parent_h = nwg::ControlHandle::Hwnd(parent as _);
        let handler = nwg::bind_event_handler(
            &nwg::ControlHandle::Hwnd(hwnd), &parent_h,
            move |evt, _data, _handle| {
                if let nwg::Event::OnButtonClick = evt {
                    if let Some(ref mut f) = *cb.borrow_mut() { f(); }
                }
            },
        );
        Ok((btn, click_cb, handler))
    }

    pub fn create_label(parent: *mut c_void) -> Result<nwg::Label, nwg::NwgError> {
        let mut lbl = nwg::Label::default();
        nwg::Label::builder()
            .text("")
            .flags(nwg::LabelFlags::VISIBLE)
            .parent(&nwg::ControlHandle::Hwnd(parent as _))
            .build(&mut lbl)?;
        Ok(lbl)
    }

    pub fn create_entry(
        parent: *mut c_void,
    ) -> Result<(nwg::TextInput, Rc<RefCell<Option<Box<dyn FnMut()>>>>, nwg::EventHandler), nwg::NwgError> {
        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
        unsafe { mark95nwg(b"n-e0\n"); }
        let mut entry = nwg::TextInput::default();
        nwg::TextInput::builder()
            .text("")
            .flags(nwg::TextInputFlags::VISIBLE | nwg::TextInputFlags::TAB_STOP | nwg::TextInputFlags::AUTO_SCROLL)
            .parent(&nwg::ControlHandle::Hwnd(parent as _))
            .build(&mut entry)?;
        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
        unsafe { mark95nwg(b"n-e1\n"); }
        let changed_cb: Rc<RefCell<Option<Box<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
        let cb = changed_cb.clone();
        let hwnd = entry.handle.hwnd().unwrap();
        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
        unsafe { mark95nwg(b"n-e2\n"); }
        let parent_h = nwg::ControlHandle::Hwnd(parent as _);
        let handler = nwg::bind_event_handler(
            &nwg::ControlHandle::Hwnd(hwnd), &parent_h,
            move |evt, _data, _handle| {
                if let nwg::Event::OnTextInput = evt {
                    if let Some(ref mut f) = *cb.borrow_mut() { f(); }
                }
            },
        );
        #[cfg(all(target_family = "rust9x", target_env = "msvc"))]
        unsafe { mark95nwg(b"n-e3\n"); }
        Ok((entry, changed_cb, handler))
    }

    // -- WM_COMMAND notification plumbing --
    //
    // NWG 1.0.13 translates `WM_COMMAND` only from its `WM_MENUSELECT` arm
    // (see `win32/window.rs`: the `match &class_name` dispatch for
    // "Button"/"Edit"/"ComboBox" is nested inside `WM_MENUSELECT =>`), so the
    // high-level `nwg::Event::OnComboxBoxSelection` / `OnButtonClick` events
    // a ComboBox click would otherwise produce are never reached: a combo
    // click is a plain `WM_COMMAND` to its *parent*, not a `WM_MENUSELECT`.
    // That is why `create_dropdown`/`create_checkbox`/`create_radiobutton`
    // used to bind an empty closure and drop the callback on the floor.
    //
    // So we decode the notification ourselves, on the parent's HWND, and fire
    // the stored callback. Two details are load-bearing:
    //
    //   * The handler id MUST be > 0xFFFF. NWG reserves the low range and
    //     `bind_raw_event_handler` panics on it (see `Window::
    //     start_repeating_timer`, which documents the same constraint).
    //   * `WM_COMMAND` carries every child's notifications, so the `lParam`
    //     child HWND must be matched or the callback fires for the wrong
    //     widget. `lParam` is zero for a menu/accelerator command, which the
    //     null check below rejects.
    static NOTIFY_HANDLER_ID: AtomicUsize = AtomicUsize::new(0x9000_0000);

    /// Bind a `WM_COMMAND` handler on `parent` that fires `cb` when `child`
    /// reports notification code `code` (e.g. `CBN_SELCHANGE`).
    ///
    /// Returns the handler so the caller can keep it alive for the control's
    /// lifetime: dropping an `nwg::RawEventHandler` unbinds the subclass.
    fn bind_command_notify(
        parent: *mut c_void,
        child: *mut c_void,
        code: u16,
        cb: Rc<RefCell<Option<Box<dyn FnMut()>>>>,
    ) -> Option<nwg::RawEventHandler> {
        if parent.is_null() || child.is_null() {
            return None;
        }
        let id = NOTIFY_HANDLER_ID.fetch_add(1, Ordering::SeqCst);
        nwg::bind_raw_event_handler(
            &nwg::ControlHandle::Hwnd(parent as _),
            id,
            move |_h, msg, w, l| {
                if msg != winapi::um::winuser::WM_COMMAND {
                    return None;
                }
                // HIWORD(w) is the notification code, LOWORD(w) the control id.
                if (winapi::shared::minwindef::HIWORD(w as u32) as u16) != code {
                    return None;
                }
                // lParam is the child HWND that sent the notification; a
                // menu/accelerator command carries 0 and must not match.
                if (l as *mut c_void) != child {
                    return None;
                }
                if let Some(ref mut f) = *cb.borrow_mut() {
                    f();
                }
                // None => DefSubclassProc; a notification must still reach the
                // control so the selection state is committed.
                None
            },
        )
        .ok()
    }

    // -- DropDown (ComboBox) --

    pub fn create_dropdown(
        parent: *mut c_void,
        items: &[&str],
    ) -> Result<(nwg::ComboBox<String>, Rc<RefCell<Option<Box<dyn FnMut()>>>>, Option<nwg::RawEventHandler>), nwg::NwgError> {
        let mut cb = nwg::ComboBox::<String>::default();
        nwg::ComboBox::builder()
            .flags(nwg::ComboBoxFlags::VISIBLE | nwg::ComboBoxFlags::TAB_STOP)
            .collection(items.iter().map(|s| s.to_string()).collect())
            .selected_index(Some(0))
            .parent(&nwg::ControlHandle::Hwnd(parent as _))
            .build(&mut cb)?;
        let hwnd = cb.handle.hwnd().unwrap();
        let changed_cb: Rc<RefCell<Option<Box<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
        let notify = bind_command_notify(
            parent,
            hwnd as *mut c_void,
            winapi::um::winuser::CBN_SELCHANGE,
            changed_cb.clone(),
        );
        Ok((cb, changed_cb, notify))
    }

    // -- CheckBox --

    pub fn create_checkbox(
        parent: *mut c_void,
        text: &str,
    ) -> Result<(nwg::CheckBox, Rc<RefCell<Option<Box<dyn FnMut()>>>>, Option<nwg::RawEventHandler>), nwg::NwgError> {
        let mut chk = nwg::CheckBox::default();
        nwg::CheckBox::builder()
            .text(text)
            .flags(nwg::CheckBoxFlags::VISIBLE | nwg::CheckBoxFlags::TAB_STOP)
            .check_state(nwg::CheckBoxState::Unchecked)
            .parent(&nwg::ControlHandle::Hwnd(parent as _))
            .build(&mut chk)?;
        let hwnd = chk.handle.hwnd().unwrap();
        let toggled_cb: Rc<RefCell<Option<Box<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
        let notify = bind_command_notify(
            parent,
            hwnd as *mut c_void,
            winapi::um::winuser::BN_CLICKED,
            toggled_cb.clone(),
        );
        Ok((chk, toggled_cb, notify))
    }

    // -- RadioButton --

    pub fn create_radiobutton(
        parent: *mut c_void,
        text: &str,
        is_group_start: bool,
    ) -> Result<(nwg::RadioButton, Rc<RefCell<Option<Box<dyn FnMut()>>>>, Option<nwg::RawEventHandler>), nwg::NwgError> {
        let mut rb = nwg::RadioButton::default();
        let mut flags = nwg::RadioButtonFlags::VISIBLE | nwg::RadioButtonFlags::TAB_STOP;
        if is_group_start {
            flags |= nwg::RadioButtonFlags::GROUP;
        }
        nwg::RadioButton::builder()
            .text(text)
            .flags(flags)
            .parent(&nwg::ControlHandle::Hwnd(parent as _))
            .build(&mut rb)?;
        let hwnd = rb.handle.hwnd().unwrap();
        let toggled_cb: Rc<RefCell<Option<Box<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
        let notify = bind_command_notify(
            parent,
            hwnd as *mut c_void,
            winapi::um::winuser::BN_CLICKED,
            toggled_cb.clone(),
        );
        Ok((rb, toggled_cb, notify))
    }

    // -- TextBox (multi-line) --

    pub fn create_textview(
        parent: *mut c_void,
    ) -> Result<(nwg::TextBox, Rc<RefCell<Option<Box<dyn FnMut()>>>>, nwg::EventHandler), nwg::NwgError> {
        let mut tv = nwg::TextBox::default();
        nwg::TextBox::builder()
            .text("")
            .flags(nwg::TextBoxFlags::VISIBLE | nwg::TextBoxFlags::AUTOVSCROLL | nwg::TextBoxFlags::TAB_STOP)
            .parent(&nwg::ControlHandle::Hwnd(parent as _))
            .build(&mut tv)?;
        let changed_cb: Rc<RefCell<Option<Box<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
        let cb = changed_cb.clone();
        let hwnd = tv.handle.hwnd().unwrap();
        let parent_h = nwg::ControlHandle::Hwnd(parent as _);
        let handler = nwg::bind_event_handler(
            &nwg::ControlHandle::Hwnd(hwnd), &parent_h,
            move |evt, _data, _handle| {
                if let nwg::Event::OnTextInput = evt {
                    if let Some(ref mut f) = *cb.borrow_mut() { f(); }
                }
            },
        );
        Ok((tv, changed_cb, handler))
    }

    // -- Dialog (window with button callbacks) --

    pub fn create_dialog(
        _parent_cell: &Rc<RefCell<Option<*mut c_void>>>,
        _button_cb: Rc<RefCell<Option<Box<dyn FnMut(i32)>>>>,
    ) -> Result<(nwg::Window, Vec<(nwg::Button, nwg::EventHandler)>, nwg::EventHandler), nwg::NwgError> {
        let mut win = nwg::Window::default();
        nwg::Window::builder()
            .flags(nwg::WindowFlags::WINDOW | nwg::WindowFlags::VISIBLE)
            .size((450, 500))
            .build(&mut win)?;
        let hwnd = win.handle.hwnd().unwrap_or(std::ptr::null_mut());
        // NOTE: deliberately does NOT overwrite parent_cell (unlike
        // create_window): dialogs are transient siblings, not the widget
        // parent. Later new_entry/new_box calls keep the main window as
        // parent until append_content_area reparents them into the dialog.
        // Overwriting here orphaned subsequently created widgets onto dead
        // dialogs.
        let handler = nwg::bind_event_handler(
            &nwg::ControlHandle::Hwnd(hwnd),
            &nwg::ControlHandle::Hwnd(hwnd),
            |_evt, _data, _handle| {},
        );
        Ok((win, Vec::new(), handler))
    }

    pub fn create_dialog_button(
        parent: *mut c_void,
        text: &str,
        response_id: i32,
        cb: Rc<RefCell<Option<Box<dyn FnMut(i32)>>>>,
    ) -> Result<(nwg::Button, nwg::EventHandler), nwg::NwgError> {
        let mut btn = nwg::Button::default();
        nwg::Button::builder()
            .text(text)
            .flags(nwg::ButtonFlags::VISIBLE | nwg::ButtonFlags::TAB_STOP)
            .parent(&nwg::ControlHandle::Hwnd(parent as _))
            .build(&mut btn)?;
        let cb2 = cb.clone();
        let rid = response_id;
        let hwnd = btn.handle.hwnd().unwrap();
        let parent_h = nwg::ControlHandle::Hwnd(parent as _);
        let handler = nwg::bind_event_handler(
            &nwg::ControlHandle::Hwnd(hwnd), &parent_h,
            move |evt, _data, _handle| {
                if let nwg::Event::OnButtonClick = evt {
                    if let Some(ref mut f) = *cb2.borrow_mut() { f(rid); }
                    // Dismiss like the Escape path: confirming must close.
                    unsafe {
                        winapi::um::winuser::ShowWindow(parent as _, winapi::um::winuser::SW_HIDE);
                    }
                }
            },
        );
        Ok((btn, handler))
    }

    // -- Menu building types (used by the adapter) --

    #[derive(Clone)]
    pub struct MenuItemData {
        pub label: String,
        pub detailed_action: String,
        pub submenu: Option<Vec<MenuItemData>>,
    }

    /// Build an NWG menu/submenu from MenuItemData slice.
    /// `parent_handle` is the ControlHandle of the parent (window or parent menu).
    /// `next_id` is a counter for assigning unique menu item IDs.
    /// Returns the list of leaf items with their IDs for action dispatching.
    pub fn build_nwg_menu_items(
        parent_handle: &nwg::ControlHandle,
        items: &[MenuItemData],
        next_id: &mut u32,
    ) -> Result<Vec<(u32, String)>, nwg::NwgError> {
        let mut leaves = Vec::new();
        for item in items {
            if let Some(ref children) = item.submenu {
                // Submenu: create a popup menu with children
                let mut sub_menu = nwg::Menu::default();
                nwg::Menu::builder()
                    .text(&format!("&{}", item.label))
                    .popup(true)
                    .parent(parent_handle.clone())
                    .build(&mut sub_menu)?;
                let sub_hmenu = sub_menu.handle.pop_hmenu().map(|(_, h)| h).unwrap_or(std::ptr::null_mut());
                // Build a temp handle for recursion — we need the PopMenu variant
                let temp_handle = match parent_handle {
                    nwg::ControlHandle::Hwnd(h) => nwg::ControlHandle::PopMenu(*h, sub_hmenu),
                    nwg::ControlHandle::PopMenu(h, _) => nwg::ControlHandle::PopMenu(*h, sub_hmenu),
                    _ => unreachable!(),
                };
                let child_leaves = build_nwg_menu_items(&temp_handle, children, next_id)?;
                leaves.extend(child_leaves);
            } else {
                // Leaf item
                let mut menu_item = nwg::MenuItem::default();
                let id = *next_id;
                *next_id += 1;
                nwg::MenuItem::builder()
                    .text(&format!("&{}", item.label))
                    .parent(parent_handle.clone())
                    .build(&mut menu_item)?;
                leaves.push((id, item.detailed_action.clone()));
            }
        }
        Ok(leaves)
    }

    #[derive(Clone, Copy)]
    pub enum Orientation {
        Horizontal = 0,
        Vertical = 1,
    }

    // -- ScrolledWindow --

    pub struct ScrolledWindowParts {
        pub frame: nwg::Frame,
        pub vscroll: nwg::ScrollBar,
        pub hscroll: nwg::ScrollBar,
    }

    pub fn create_scrolled_window(parent: *mut c_void) -> Result<ScrolledWindowParts, nwg::NwgError> {
        let mut frame = nwg::Frame::default();
        nwg::Frame::builder()
            .flags(nwg::FrameFlags::VISIBLE)
            .parent(&nwg::ControlHandle::Hwnd(parent as _))
            .build(&mut frame)?;

        let mut vscroll = nwg::ScrollBar::default();
        nwg::ScrollBar::builder()
            .flags(nwg::ScrollBarFlags::VISIBLE | nwg::ScrollBarFlags::VERTICAL)
            .parent(&nwg::ControlHandle::Hwnd(parent as _))
            .range(Some(0..100))
            .pos(Some(0))
            .build(&mut vscroll)?;

        let mut hscroll = nwg::ScrollBar::default();
        nwg::ScrollBar::builder()
            .flags(nwg::ScrollBarFlags::VISIBLE | nwg::ScrollBarFlags::HORIZONTAL)
            .parent(&nwg::ControlHandle::Hwnd(parent as _))
            .range(Some(0..100))
            .pos(Some(0))
            .build(&mut hscroll)?;

        Ok(ScrolledWindowParts { frame, vscroll, hscroll })
    }
}

#[cfg(windows)]
pub use nwg_backend::*;
