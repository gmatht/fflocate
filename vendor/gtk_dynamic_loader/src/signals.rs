use std::ffi::CString;
use std::os::raw::c_void;

/// Log a panic caught at the GTK FFI boundary (where unwinding into C
/// would abort the process with no message). Returns so callers can supply
/// a safe default and keep the app alive.
fn log_trampoline_panic(what: &str, err: Box<dyn std::any::Any + Send>) {
    let msg = if let Some(s) = err.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = err.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    };
    let line = format!("corro: panic in GTK callback ({what}): {msg}\n");
    let _ = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/corro-panic.log").and_then(|mut f| { use std::io::Write as _; writeln!(f, "{line}") });
    eprintln!("{line}");
}

// Trampoline and destroy notify for clicked handler
// We'll define a small C-ABI trampoline that converts user_data pointer into a Box<dyn FnMut()>

// trampoline for signals with (instance, user_data)
#[no_mangle]
pub extern "C" fn gtk_compat_trampoline_2(_instance: *mut c_void, user_data: *mut c_void) {
    unsafe {
        if user_data.is_null() { return; }
        let inner_ptr = user_data as *mut Box<dyn FnMut()>;
        if inner_ptr.is_null() { return; }
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let closure_ref: &mut dyn FnMut() = &mut **inner_ptr;
            closure_ref();
        }));
        if let Err(err) = r {
            log_trampoline_panic("trampoline_2", err);
        }
    }
}

// trampoline for GTK4 EventControllerFocus enter/leave — calls FnMut(*mut c_void)->i32 with null
#[no_mangle]
pub extern "C" fn gtk_compat_trampoline_focus(_instance: *mut c_void, user_data: *mut c_void) -> i32 {
    unsafe {
        if user_data.is_null() { return 0; }
        let inner_ptr = user_data as *mut Box<dyn FnMut(*mut c_void) -> i32>;
        if inner_ptr.is_null() { return 0; }
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let closure_ref: &mut dyn FnMut(*mut c_void) -> i32 = &mut **inner_ptr;
            closure_ref(std::ptr::null_mut())
        }));
        match r {
            Ok(v) => v,
            Err(err) => {
                log_trampoline_panic("trampoline_focus", err);
                0
            }
        }
    }
}

pub unsafe extern "C" fn gtk_compat_destroy_notify_focus(_data: *mut c_void, _closure: *mut c_void) {
    if !_data.is_null() {
        drop(Box::from_raw(_data as *mut Box<dyn FnMut(*mut c_void) -> i32>));
    }
}

// trampoline for signals with (instance, param, user_data)
#[no_mangle]
pub extern "C" fn gtk_compat_trampoline_3(_instance: *mut c_void, _param: *mut c_void, user_data: *mut c_void) {
    unsafe {
        if user_data.is_null() { return; }
        let inner_ptr = user_data as *mut Box<dyn FnMut()>;
        if inner_ptr.is_null() { return; }
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let closure_ref: &mut dyn FnMut() = &mut **inner_ptr;
            closure_ref();
        }));
        if let Err(err) = r {
            log_trampoline_panic("trampoline_3", err);
        }
    }
}

#[no_mangle]
pub extern "C" fn gtk_compat_destroy_notify(data: *mut c_void, _closure: *mut c_void) {
    unsafe {
        if data.is_null() { return; }
        // data is a *mut Box<dyn FnMut()> produced by Box::into_raw
        let inner_ptr = data as *mut Box<dyn FnMut()>;
        // reconstruct the outer Box<Box<dyn FnMut()>> and drop it (frees the closure)
        let _boxed: Box<Box<dyn FnMut()>> = Box::from_raw(inner_ptr);
        // dropped here
    }
}

// trampoline for signals that pass a param pointer to the closure (no return)
#[no_mangle]
pub extern "C" fn gtk_compat_trampoline_param(_instance: *mut c_void, param: *mut c_void, user_data: *mut c_void) {
    unsafe {
        if user_data.is_null() { return; }
        let inner_ptr = user_data as *mut Box<dyn FnMut(*mut c_void)>;
        if inner_ptr.is_null() { return; }
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let closure_ref: &mut dyn FnMut(*mut c_void) = &mut **inner_ptr;
            closure_ref(param);
        }));
        if let Err(err) = r {
            log_trampoline_panic("trampoline_param", err);
        }
    }
}

#[no_mangle]
pub extern "C" fn gtk_compat_destroy_notify_param(data: *mut c_void, _closure: *mut c_void) {
    unsafe {
        if data.is_null() { return; }
        let inner_ptr = data as *mut Box<dyn FnMut(*mut c_void)>;
        let _boxed: Box<Box<dyn FnMut(*mut c_void)>> = Box::from_raw(inner_ptr);
    }
}

// trampoline for signals that expect a gboolean/int return value (e.g. key-press-event)
#[no_mangle]
pub extern "C" fn gtk_compat_trampoline_bool(_instance: *mut c_void, param: *mut c_void, user_data: *mut c_void) -> i32 {
    unsafe {
        if user_data.is_null() { return 0; }
        let inner_ptr = user_data as *mut Box<dyn FnMut(*mut c_void) -> i32>;
        if inner_ptr.is_null() { return 0; }
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let closure_ref: &mut dyn FnMut(*mut c_void) -> i32 = &mut **inner_ptr;
            closure_ref(param)
        }));
        match r {
            Ok(v) => v,
            Err(err) => {
                log_trampoline_panic("trampoline_bool", err);
                0
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn gtk_compat_destroy_notify_bool(data: *mut c_void, _closure: *mut c_void) {
    unsafe {
        if data.is_null() { return; }
        let inner_ptr = data as *mut Box<dyn FnMut(*mut c_void) -> i32>;
        let _boxed: Box<Box<dyn FnMut(*mut c_void) -> i32>> = Box::from_raw(inner_ptr);
    }
}

// helper to connect a "clicked" handler using either g_signal_connect_data or g_signal_connect
pub unsafe fn connect_signal(lib_symbols: &crate::symbols::Symbols, instance: *mut c_void, signal_name: &str, cb: Box<dyn FnMut()>, arity: u8) -> Result<u64, String> {
    // box twice so the pointer to Box remains stable (Box<dyn FnMut()> -> *mut Box<dyn FnMut()>)
    let boxed: Box<Box<dyn FnMut()>> = Box::new(Box::new(cb));
    let raw = Box::into_raw(boxed) as *mut c_void;

    let sig_name = CString::new(signal_name).unwrap();
    if let Some(gscd) = lib_symbols.g_signal_connect_data {
        // connect with destroy notify
        let handler_ptr = match arity {
            3 => gtk_compat_trampoline_3 as *const () as *mut c_void,
            _ => gtk_compat_trampoline_2 as *const () as *mut c_void,
        };
        let destroy_ptr = Some(gtk_compat_destroy_notify as unsafe extern "C" fn(*mut c_void, *mut c_void));
        let id = gscd(instance, sig_name.as_ptr() as *const u8, handler_ptr, raw, destroy_ptr, 0);
        Ok(id)
    } else if let Some(gsc) = lib_symbols.g_signal_connect {
        let handler_ptr = match arity {
            3 => gtk_compat_trampoline_3 as *const () as *mut c_void,
            _ => gtk_compat_trampoline_2 as *const () as *mut c_void,
        };
        let id = gsc(instance, sig_name.as_ptr() as *const u8, handler_ptr, raw);
        // We didn't register a destroy notify; closure will leak. It's acceptable for the demo.
        Ok(id)
    } else {
        Err("no g_signal_connect available".into())
    }
}

// Connect a signal where the closure wants the raw param pointer (no return value)
pub unsafe fn connect_signal_param(lib_symbols: &crate::symbols::Symbols, instance: *mut c_void, signal_name: &str, cb: Box<dyn FnMut(*mut c_void)>) -> Result<u64, String> {
    // box twice so the pointer to Box remains stable
    let boxed: Box<Box<dyn FnMut(*mut c_void)>> = Box::new(Box::new(cb));
    let raw = Box::into_raw(boxed) as *mut c_void;
    let sig_name = CString::new(signal_name).unwrap();
    if let Some(gscd) = lib_symbols.g_signal_connect_data {
        let handler_ptr = gtk_compat_trampoline_param as *const () as *mut c_void;
        let destroy_ptr = Some(gtk_compat_destroy_notify_param as unsafe extern "C" fn(*mut c_void, *mut c_void));
        let id = gscd(instance, sig_name.as_ptr() as *const u8, handler_ptr, raw, destroy_ptr, 0);
        Ok(id)
    } else if let Some(gsc) = lib_symbols.g_signal_connect {
        let handler_ptr = gtk_compat_trampoline_param as *const () as *mut c_void;
        let id = gsc(instance, sig_name.as_ptr() as *const u8, handler_ptr, raw);
        Ok(id)
    } else { Err("no g_signal_connect available".into()) }
}

// trampoline for gesture signals (e.g. GtkGestureClick::pressed/released) with (n_press, x, y, user_data)
#[no_mangle]
pub extern "C" fn gtk_compat_trampoline_gesture(_instance: *mut c_void, n_press: i32, x: f64, y: f64, user_data: *mut c_void) {
    unsafe {
        if user_data.is_null() { return; }
        let inner_ptr = user_data as *mut Box<dyn FnMut(i32, f64, f64)>;
        if inner_ptr.is_null() { return; }
        let closure_ref: &mut dyn FnMut(i32, f64, f64) = &mut **inner_ptr;
        closure_ref(n_press, x, y);
    }
}

#[no_mangle]
pub extern "C" fn gtk_compat_destroy_notify_gesture(data: *mut c_void, _closure: *mut c_void) {
    unsafe {
        if data.is_null() { return; }
        let inner_ptr = data as *mut Box<dyn FnMut(i32, f64, f64)>;
        let _boxed: Box<Box<dyn FnMut(i32, f64, f64)>> = Box::from_raw(inner_ptr);
    }
}

pub unsafe fn connect_signal_gesture(lib_symbols: &crate::symbols::Symbols, instance: *mut c_void, signal_name: &str, cb: Box<dyn FnMut(i32, f64, f64)>) -> Result<u64, String> {
    let boxed: Box<Box<dyn FnMut(i32, f64, f64)>> = Box::new(Box::new(cb));
    let raw = Box::into_raw(boxed) as *mut c_void;
    let sig_name = CString::new(signal_name).unwrap();
    if let Some(gscd) = lib_symbols.g_signal_connect_data {
        let handler_ptr = gtk_compat_trampoline_gesture as *const () as *mut c_void;
        let destroy_ptr = Some(gtk_compat_destroy_notify_gesture as unsafe extern "C" fn(*mut c_void, *mut c_void));
        let id = gscd(instance, sig_name.as_ptr() as *const u8, handler_ptr, raw, destroy_ptr, 0);
        Ok(id)
    } else if let Some(gsc) = lib_symbols.g_signal_connect {
        let handler_ptr = gtk_compat_trampoline_gesture as *const () as *mut c_void;
        let id = gsc(instance, sig_name.as_ptr() as *const u8, handler_ptr, raw);
        Ok(id)
    } else { Err("no g_signal_connect available".into()) }
}

// trampoline for motion signals (e.g. GtkEventControllerMotion::motion) with (x, y, user_data)
#[no_mangle]
pub extern "C" fn gtk_compat_trampoline_motion(_instance: *mut c_void, x: f64, y: f64, user_data: *mut c_void) {
    unsafe {
        if user_data.is_null() { return; }
        let inner_ptr = user_data as *mut Box<dyn FnMut(f64, f64)>;
        if inner_ptr.is_null() { return; }
        let closure_ref: &mut dyn FnMut(f64, f64) = &mut **inner_ptr;
        closure_ref(x, y);
    }
}

#[no_mangle]
pub extern "C" fn gtk_compat_destroy_notify_motion(data: *mut c_void, _closure: *mut c_void) {
    unsafe {
        if data.is_null() { return; }
        let inner_ptr = data as *mut Box<dyn FnMut(f64, f64)>;
        let _boxed: Box<Box<dyn FnMut(f64, f64)>> = Box::from_raw(inner_ptr);
    }
}

pub unsafe fn connect_signal_motion(lib_symbols: &crate::symbols::Symbols, instance: *mut c_void, signal_name: &str, cb: Box<dyn FnMut(f64, f64)>) -> Result<u64, String> {
    let boxed: Box<Box<dyn FnMut(f64, f64)>> = Box::new(Box::new(cb));
    let raw = Box::into_raw(boxed) as *mut c_void;
    let sig_name = CString::new(signal_name).unwrap();
    if let Some(gscd) = lib_symbols.g_signal_connect_data {
        let handler_ptr = gtk_compat_trampoline_motion as *const () as *mut c_void;
        let destroy_ptr = Some(gtk_compat_destroy_notify_motion as unsafe extern "C" fn(*mut c_void, *mut c_void));
        let id = gscd(instance, sig_name.as_ptr() as *const u8, handler_ptr, raw, destroy_ptr, 0);
        Ok(id)
    } else if let Some(gsc) = lib_symbols.g_signal_connect {
        let handler_ptr = gtk_compat_trampoline_motion as *const () as *mut c_void;
        let id = gsc(instance, sig_name.as_ptr() as *const u8, handler_ptr, raw);
        Ok(id)
    } else { Err("no g_signal_connect available".into()) }
}

// Connect a signal that expects a gboolean/int return from the handler
pub unsafe fn connect_signal_bool(lib_symbols: &crate::symbols::Symbols, instance: *mut c_void, signal_name: &str, cb: Box<dyn FnMut(*mut c_void) -> i32>) -> Result<u64, String> {
    let boxed: Box<Box<dyn FnMut(*mut c_void) -> i32>> = Box::new(Box::new(cb));
    let raw = Box::into_raw(boxed) as *mut c_void;
    let sig_name = CString::new(signal_name).unwrap();
    if let Some(gscd) = lib_symbols.g_signal_connect_data {
        let handler_ptr = gtk_compat_trampoline_bool as *const () as *mut c_void;
        let destroy_ptr = Some(gtk_compat_destroy_notify_bool as unsafe extern "C" fn(*mut c_void, *mut c_void));
        let id = gscd(instance, sig_name.as_ptr() as *const u8, handler_ptr, raw, destroy_ptr, 0);
        Ok(id)
    } else if let Some(gsc) = lib_symbols.g_signal_connect {
        let handler_ptr = gtk_compat_trampoline_bool as *const () as *mut c_void;
        let id = gsc(instance, sig_name.as_ptr() as *const u8, handler_ptr, raw);
        Ok(id)
    } else { Err("no g_signal_connect available".into()) }
}

// trampoline for GTK3 "draw" signal — passes (widget, cairo_t*) to closure returning gboolean
#[no_mangle]
pub extern "C" fn gtk_compat_trampoline_draw_gtk3(instance: *mut c_void, cr: *mut c_void, user_data: *mut c_void) -> i32 {
    unsafe {
        if user_data.is_null() { return 0; }
        let inner_ptr = user_data as *mut Box<dyn FnMut(*mut c_void, *mut c_void) -> i32>;
        if inner_ptr.is_null() { return 0; }
        let closure_ref: &mut dyn FnMut(*mut c_void, *mut c_void) -> i32 = &mut **inner_ptr;
        closure_ref(instance, cr)
    }
}

#[no_mangle]
pub extern "C" fn gtk_compat_destroy_notify_draw_gtk3(data: *mut c_void, _closure: *mut c_void) {
    unsafe {
        if data.is_null() { return; }
        let inner_ptr = data as *mut Box<dyn FnMut(*mut c_void, *mut c_void) -> i32>;
        let _boxed: Box<Box<dyn FnMut(*mut c_void, *mut c_void) -> i32>> = Box::from_raw(inner_ptr);
    }
}

// trampoline for GTK4 GtkEventControllerKey::key-pressed signal
// passes (keyval, keycode, state) as u32 triple — NOT an event pointer
#[no_mangle]
pub extern "C" fn gtk_compat_trampoline_key_pressed(_instance: *mut c_void, keyval: u32, _keycode: u32, _state: u32, user_data: *mut c_void) -> i32 {
    unsafe {
        if user_data.is_null() { return 0; }
        let inner_ptr = user_data as *mut Box<dyn FnMut(u32) -> i32>;
        if inner_ptr.is_null() { return 0; }
        let closure_ref: &mut dyn FnMut(u32) -> i32 = &mut **inner_ptr;
        closure_ref(keyval)
    }
}

#[no_mangle]
pub extern "C" fn gtk_compat_destroy_notify_key_pressed(data: *mut c_void, _closure: *mut c_void) {
    unsafe {
        if data.is_null() { return; }
        let inner_ptr = data as *mut Box<dyn FnMut(u32) -> i32>;
        let _boxed: Box<Box<dyn FnMut(u32) -> i32>> = Box::from_raw(inner_ptr);
    }
}

// trampoline for GTK4 GtkEventControllerKey::key-released.
//
// Same signature as `key-pressed` (GtkEventControllerKey emits both from the
// same controller), and needed because GTK4 delivers a physical key as BOTH a
// `key-pressed` and a `key-released` signal. Without a handler for the release
// signal, a host that only hooks `key-pressed` sees each key twice on the
// versions/display servers where the release is routed through the same
// handler — the bug `gui_backend.rs` used to paper over with consecutive-keyval
// heuristics. Connecting this separately lets the adapter swallow releases and
// hand the host a pure press stream.
#[no_mangle]
pub extern "C" fn gtk_compat_trampoline_key_released(_instance: *mut c_void, keyval: u32, _keycode: u32, _state: u32, user_data: *mut c_void) -> i32 {
    unsafe {
        if user_data.is_null() { return 0; }
        let inner_ptr = user_data as *mut Box<dyn FnMut(u32) -> i32>;
        if inner_ptr.is_null() { return 0; }
        let closure_ref: &mut dyn FnMut(u32) -> i32 = &mut **inner_ptr;
        closure_ref(keyval)
    }
}

#[no_mangle]
pub extern "C" fn gtk_compat_destroy_notify_key_released(data: *mut c_void, _closure: *mut c_void) {
    unsafe {
        if data.is_null() { return; }
        let inner_ptr = data as *mut Box<dyn FnMut(u32) -> i32>;
        let _boxed: Box<Box<dyn FnMut(u32) -> i32>> = Box::from_raw(inner_ptr);
    }
}

// trampoline for GTK4 gtk_drawing_area_set_draw_func — passes (cr, width, height)
#[no_mangle]
pub extern "C" fn gtk_compat_trampoline_draw_gtk4(_area: *mut c_void, cr: *mut c_void, w: i32, h: i32, user_data: *mut c_void) {
    unsafe {
        if user_data.is_null() { return; }
        let inner_ptr = user_data as *mut Box<dyn FnMut(*mut c_void, i32, i32)>;
        if inner_ptr.is_null() { return; }
        let closure_ref: &mut dyn FnMut(*mut c_void, i32, i32) = &mut **inner_ptr;
        closure_ref(cr, w, h);
    }
}

#[no_mangle]
pub extern "C" fn gtk_compat_destroy_notify_draw_gtk4(data: *mut c_void, _closure: *mut c_void) {
    unsafe {
        if data.is_null() { return; }
        let inner_ptr = data as *mut Box<dyn FnMut(*mut c_void, i32, i32)>;
        let _boxed: Box<Box<dyn FnMut(*mut c_void, i32, i32)>> = Box::from_raw(inner_ptr);
    }
}

#[cfg(test)]
mod key_release_tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// The `key-released` trampoline must deliver the keyval to the closure.
    ///
    /// This is the contract the GTK4 release filter relies on: the adapter
    /// connects `key-released` and returns GDK_EVENT_STOP without touching the
    /// host, so if the trampoline ever stopped delivering the keyval the
    /// filter would silently become a no-op and hosts would see double keys
    /// again (the bug it exists to prevent).
    #[test]
    fn key_released_trampoline_delivers_the_keyval() {
        let seen: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));
        let s = seen.clone();
        let cb: Box<dyn FnMut(u32) -> i32> = Box::new(move |kv: u32| {
            s.borrow_mut().push(kv);
            1
        });
        let raw = Box::into_raw(Box::new(cb)) as *mut c_void;
        let rc = gtk_compat_trampoline_key_released(std::ptr::null_mut(), 0xff52, 0, 0, raw);
        assert_eq!(1, rc, "trampoline returns the closure's value");
        assert_eq!(vec![0xff52], *seen.borrow());
        // The destroy notify must free the box without double-freeing: run it
        // once (the trampoline itself must not consume the box).
        gtk_compat_destroy_notify_key_released(raw, std::ptr::null_mut());
    }

    /// A null user_data must not crash (GObject can deliver to a dropped
    /// closure during teardown).
    #[test]
    fn key_released_trampoline_tolerates_null() {
        assert_eq!(0, gtk_compat_trampoline_key_released(std::ptr::null_mut(), 0x41, 0, 0, std::ptr::null_mut()));
    }
}
