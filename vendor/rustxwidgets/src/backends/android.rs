#[cfg(target_os = "android")]
mod android_backend {
    use jni::objects::{GlobalRef, JObject, JString};
    use jni::JNIEnv;
    use once_cell::sync::OnceCell;
    use once_cell::sync::Lazy;
    use std::collections::HashMap;
    use std::error::Error as StdError;
    use std::sync::Mutex;

    static JAVA_VM: OnceCell<jni::JavaVM> = OnceCell::new();
    /// The app's ClassLoader, captured while we are still on the Activity's
    /// thread. `JNIEnv::find_class` on a JNI-attached thread resolves through
    /// the *system* loader and cannot see APK classes (`com.corro.*`), so the
    /// app loader is cached here and used by `load_app_class`.
    static APP_CLASS_LOADER: OnceCell<GlobalRef> = OnceCell::new();

    /// Keeps GlobalRefs alive so raw jobject pointers remain valid.
    static KEEP_ALIVE: Lazy<Mutex<Vec<GlobalRef>>> = Lazy::new(|| Mutex::new(Vec::new()));

    /// Create a GlobalRef from a local JObject, store it, and return the raw pointer.
    pub fn make_global_ref(env: &mut JNIEnv, obj: &JObject<'_>) -> Result<jni::sys::jobject, Box<dyn StdError + Send + Sync>> {
        let gref = env.new_global_ref(obj)?;
        let raw = gref.as_obj().as_raw();
        KEEP_ALIVE.lock().unwrap().push(gref);
        Ok(raw)
    }

    pub fn init(env: &mut JNIEnv, activity: &JObject<'_>) -> Result<(), Box<dyn StdError + Send + Sync>> {
        let content_id: jni::sys::jint = env.get_static_field(
            "android/R$id",
            "content",
            "I",
        )?.i()?;

        let root_view = env.call_method(
            activity,
            "findViewById",
            "(I)Landroid/view/View;",
            &[content_id.into()],
        )?;
        let root_view = root_view.l()?;
        init_with_layout(env, activity, &root_view)
    }

    /// Mutable handle cells: a re-init (Activity recreated after rotation)
    /// replaces them, which `OnceCell` could not do.
    fn activity_cell() -> &'static Mutex<Option<GlobalRef>> {
        static CELL: OnceCell<Mutex<Option<GlobalRef>>> = OnceCell::new();
        CELL.get_or_init(|| Mutex::new(None))
    }

    fn root_layout_cell() -> &'static Mutex<Option<GlobalRef>> {
        static CELL: OnceCell<Mutex<Option<GlobalRef>>> = OnceCell::new();
        CELL.get_or_init(|| Mutex::new(None))
    }

    /// Like init() but accepts an explicitly provided root layout ViewGroup.
    /// Avoids the JNI lookup of android.R.id.content.
    ///
    /// Idempotent: a second call (an Activity recreated after rotation, or
    /// the launcher re-entering a live task) re-captures the new
    /// Activity/layout handles instead of failing on the one-shot cells.
    /// The JVM never changes for a process, so its check is only a
    /// consistency assertion — mismatched VMs would be a host bug.
    pub fn init_with_layout(env: &mut JNIEnv, activity: &JObject<'_>, layout: &JObject<'_>) -> Result<(), Box<dyn StdError + Send + Sync>> {
        let vm = env.get_java_vm()?;
        let activity_ref = env.new_global_ref(activity)?;
        let root_ref = env.new_global_ref(layout)?;

        // The JVM never changes for a process; a second call just re-captures
        // the Activity/layout handles (see the doc note above).
        let _ = JAVA_VM.set(vm);

        // Re-capture the app's ClassLoader (first init, or after recreation).
        if let Ok(loader) = env
            .call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
            .and_then(|l| l.l())
            .and_then(|obj| env.new_global_ref(&obj))
        {
            let _ = APP_CLASS_LOADER.set(loader);
            logcat_rs("APP_CLASS_LOADER captured");
        } else {
            logcat_rs("APP_CLASS_LOADER capture FAILED");
        }

        // OnceCell has no `replace`; a re-init must swap the handles for the
        // new Activity. Storing the GlobalRef in a Mutex lets the second and
        // later calls overwrite the first.
        *activity_cell().lock().unwrap() = Some(activity_ref);
        *root_layout_cell().lock().unwrap() = Some(root_ref);

        Ok(())
    }

    /// Resolve an app (APK) class by binary name (`com.corro.Foo`) through
    /// the cached app ClassLoader. Falls back to `JNIEnv::find_class` when
    /// no loader was captured (e.g. framework classes still resolve).
    pub fn load_app_class<'a>(
        env: &mut JNIEnv<'a>,
        class_name: &str,
    ) -> Result<jni::objects::JClass<'a>, Box<dyn StdError + Send + Sync>> {
        if let Some(loader) = APP_CLASS_LOADER.get() {
            let jname = env.new_string(class_name.replace('/', "."))?;
            let cls = env.call_method(
                loader.as_obj(),
                "loadClass",
                "(Ljava/lang/String;)Ljava/lang/Class;",
                &[(&jname).into()],
            )?;
            let obj = cls.l()?;
            // Class<...> is a JClass in jni's type model.
            let cls: jni::objects::JClass = jni::objects::JClass::from(obj);
            return Ok(cls);
        }
        Ok(env.find_class(class_name)?)
    }

    pub fn root_layout() -> Result<GlobalRef, Box<dyn StdError + Send + Sync>> {
        root_layout_cell()
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "ROOT_LAYOUT not initialized (call init first)".into())
    }

    pub fn activity_ref() -> Result<GlobalRef, Box<dyn StdError + Send + Sync>> {
        activity_cell()
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "ACTIVITY not initialized (call init first)".into())
    }

    pub fn with_env_and_activity<F, T>(f: F) -> Result<T, Box<dyn StdError + Send + Sync>>
    where
        F: FnOnce(&mut JNIEnv<'_>, &GlobalRef) -> Result<T, Box<dyn StdError + Send + Sync>>,
    {
        let vm = JAVA_VM.get().ok_or_else(|| "JAVA_VM not initialized (call init first)")?;
        // Clone the handle out of the Mutex so `f` borrows it: a re-init
        // (Activity recreated after rotation) swaps the cell, and holding the
        // guard across the callback would deadlock on that re-entry.
        let activity = activity_ref()?;

        let mut env = vm.attach_current_thread()?;
        f(&mut env, &activity)
    }

    pub fn is_initialized() -> bool {
        JAVA_VM.get().is_some()
    }

    /// Write a debug line to logcat under the `rswidgets` tag. Best-effort:
    /// silently dropped when the backend is not initialised yet.
    pub fn logcat_rs(msg: &str) {
        let _ = with_env_and_activity(|env, _activity| {
            let log_cls = env.find_class("android/util/Log")?;
            let tag = env.new_string("rswidgets")?;
            let jmsg = env.new_string(msg)?;
            env.call_static_method(
                &log_cls,
                "d",
                "(Ljava/lang/String;Ljava/lang/String;)I",
                &[(&tag).into(), (&jmsg).into()],
            )?;
            Ok::<_, Box<dyn StdError + Send + Sync>>(())
        });
    }

    // ---- Callback registry ----

    static CALLBACKS: Lazy<Mutex<HashMap<u64, Box<dyn FnMut() + Send>>>> = Lazy::new(|| Mutex::new(HashMap::new()));
    static NEXT_CALLBACK_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

    pub fn register_callback(f: Box<dyn FnMut() + Send>) -> u64 {
        let id = NEXT_CALLBACK_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut map = CALLBACKS.lock().unwrap();
        map.insert(id, f);
        id
    }

    // Callbacks whose signature carries **no `Send` bound** — which is what
    // every `connect_changed` / `connect_toggled` / `connect_response` in the
    // adapter takes, so they cannot use `register_callback`.
    //
    // A thread-local registry rather than a `Mutex<HashMap<..>>`, and rather
    // than an `unsafe impl Send` shim. Both alternatives would assert a bound
    // the compiler cannot check; this one asserts nothing, because every one
    // of these callbacks is dispatched from a platform listener on the UI
    // thread that built the widget (ANDROID_GUIDELINES.md §4) and never from
    // another thread. A `Mutex` would only be needed if that ever changed —
    // and `Lazy` of a `!Send` type is exactly what the compiler is protecting
    // against here.
    thread_local! {
        static LOCAL_CALLBACKS: std::cell::RefCell<HashMap<u64, Box<dyn FnMut()>>> =
            std::cell::RefCell::new(HashMap::new());
    }
    static NEXT_LOCAL_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1 << 32);

    /// Register a UI-thread callback; see [`LOCAL_CALLBACKS`].
    pub fn register_local_callback(f: impl FnMut() + 'static) -> u64 {
        let id = NEXT_LOCAL_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        LOCAL_CALLBACKS.with(|map| {
            map.borrow_mut().insert(id, Box::new(f));
        });
        id
    }

    /// Run a callback registered with [`register_local_callback`].
    ///
    /// The `try_borrow_mut` is what lets a callback re-enter the registry (a
    /// change handler that closes a dialog fires a response) without
    /// panicking: the re-entrant call is dropped instead.
    pub fn invoke_local_callback(id: u64) {
        LOCAL_CALLBACKS.with(|map| {
            let Ok(mut map) = map.try_borrow_mut() else {
                return;
            };
            if let Some(f) = map.get_mut(&id) {
                f();
            }
        });
    }

    /// Drop a callback registered with [`register_local_callback`].
    pub fn unregister_local_callback(id: u64) {
        LOCAL_CALLBACKS.with(|map| {
            map.borrow_mut().remove(&id);
        });
    }

    pub fn unregister_callback(id: u64) {
        let mut map = CALLBACKS.lock().unwrap();
        map.remove(&id);
    }

    pub fn invoke_callback(id: u64) {
        let mut map = CALLBACKS.lock().unwrap();
        if let Some(f) = map.get_mut(&id) {
            f();
        }
    }

    pub fn dispatch_callback(id: u64) {
        invoke_callback(id);
    }

    /// Create a JNI View.OnClickListener that dispatches to the Rust callback registry.
    /// This looks for a user-provided `RustCallback` class (see example project).
    /// Returns Ok(listener) if the class exists, or Ok(None) if not available.
    pub fn try_create_onclick_listener<'a>(
        env: &mut JNIEnv<'a>,
        callback_id: u64,
    ) -> Result<Option<JObject<'a>>, Box<dyn StdError + Send + Sync>> {
        let cls = match load_app_class(env, "com/example/RustCallback") {
            Ok(c) => c,
            Err(_) => {
                let _ = env.exception_clear();
                return Ok(None);
            }
        };
        let listener = env.new_object(
            &cls,
            "(J)V",
            &[(callback_id as i64).into()],
        )?;
        Ok(Some(listener))
    }

    // ---- AndroidApp ----

    pub struct AndroidApp;

    impl AndroidApp {
        pub fn new() -> Result<Box<dyn crate::backends::BackendApp>, Box<dyn StdError + Send + Sync>> {
            Ok(Box::new(AndroidApp))
        }
    }

    impl crate::backends::BackendApp for AndroidApp {
        fn run(self: Box<Self>) -> Result<(), Box<dyn StdError + Send + Sync>> {
            Ok(())
        }
    }

    pub fn init_backend() -> Result<Box<dyn crate::backends::BackendApp>, Box<dyn StdError + Send + Sync>> {
        AndroidApp::new()
    }

    // ---- Factory functions ----

    pub fn create_window() -> Result<jni::sys::jobject, Box<dyn StdError + Send + Sync>> {
        let activity = activity_ref()?;
        Ok(activity.as_obj().as_raw())
    }

    pub fn create_button(label: &str) -> Result<jni::sys::jobject, Box<dyn StdError + Send + Sync>> {
        with_env_and_activity(|env, activity| {
            let ctx = activity.as_obj();
            let btn = env.new_object(
                "android/widget/Button",
                "(Landroid/content/Context;)V",
                &[(&ctx).into()],
            )?;
            let j_label = env.new_string(label)?;
            env.call_method(
                &btn,
                "setText",
                "(Ljava/lang/CharSequence;)V",
                &[(&j_label).into()],
            )?;
            make_global_ref(env, &btn)
        })
    }

    /// Build the app's menu strip: a `com.corro.MenuStrip` holding the
    /// title, the inline quick-action buttons and the overflow button. The
    /// class is an app (APK) class, so it is resolved through the app
    /// ClassLoader, and it must expose `(Context, String)`.
    ///
    /// Returns `None` when the class is missing, so a host that does not
    /// ship one still gets a working (menu-less) UI rather than a crash.
    pub fn create_menu_strip(title: &str) -> Result<jni::sys::jobject, Box<dyn StdError + Send + Sync>> {
        with_env_and_activity(|env, activity| {
            let ctx = activity.as_obj();
            let class = load_app_class(env, "com.corro.MenuStrip")?;
            let j_title = env.new_string(title)?;
            let strip = env.new_object(
                &class,
                "(Landroid/content/Context;Ljava/lang/String;)V",
                &[(&ctx).into(), (&j_title).into()],
            )?;
            let raw = make_global_ref(env, &strip)?;
            *MENU_STRIP.lock().unwrap() = env.new_global_ref(&strip).ok();
            Ok(raw)
        })
    }

    /// Append one top-level menu (label plus alternating item label/action
    /// name) to a strip built by [`create_menu_strip`].
    pub fn menu_strip_add_menu(
        strip_ptr: *mut std::os::raw::c_void,
        label: &str,
        actions: &[&str],
    ) {
        if strip_ptr.is_null() {
            return;
        }
        let _ = with_env_and_activity(|env, _activity| {
            let strip =
                unsafe { jni::objects::JObject::from_raw(strip_ptr as jni::sys::jobject) };
            let j_label = env.new_string(label)?;
            let arr = env.new_object_array(
                actions.len() as i32,
                "java/lang/String",
                jni::objects::JObject::null(),
            )?;
            for (i, a) in actions.iter().enumerate() {
                let ja = env.new_string(a)?;
                env.set_object_array_element(&arr, i as i32, &ja)?;
            }
            let arr_obj = jni::objects::JObject::from(arr);
            env.call_method(
                &strip,
                "addMenu",
                "(Ljava/lang/String;[Ljava/lang/String;)V",
                &[(&j_label).into(), (&arr_obj).into()],
            )?;
            Ok::<_, Box<dyn StdError + Send + Sync>>(())
        });
    }

    /// Open the menu strip's overflow popup with `label` expanded.
    ///
    /// The Java side ignores an unknown label, so a mnemonic for a menu the
    /// strip does not have does nothing rather than opening an unrelated
    /// menu.
    pub fn menu_strip_open_overflow(label: &str) -> bool {
        let opened = with_env_and_activity(|env, _activity| {
            let strip = MENU_STRIP
                .lock()
                .unwrap()
                .clone()
                .ok_or("no menu strip")?;
            let j_label = env.new_string(label)?;
            env.call_method(
                strip.as_obj(),
                "showOverflowFor",
                "(Ljava/lang/String;)V",
                &[(&j_label).into()],
            )?;
            Ok::<bool, Box<dyn StdError + Send + Sync>>(true)
        });
        // A missing strip (a host without one) is `false`, which the caller
        // surfaces as an unavailable status rather than pretending a menu
        // opened.
        opened.unwrap_or(false)
    }

    /// Close the strip's overflow popup, if open.
    pub fn menu_strip_close_overflow() {
        let strip = MENU_STRIP.lock().unwrap().clone();
        let Some(strip) = strip else { return };
        let _ = with_env_and_activity(|env, _activity| {
            env.call_method(strip.as_obj(), "closeOverflow", "()V", &[])?;
            Ok::<(), Box<dyn StdError + Send + Sync>>(())
        });
    }

    /// The most recently built menu strip. There is one, because an Activity
    /// has one menu bar; a second `create_menu_strip` replaces the handle
    /// (an Activity recreated after rotation builds a new one).
    static MENU_STRIP: Lazy<Mutex<Option<GlobalRef>>> = Lazy::new(|| Mutex::new(None));

    pub fn create_label(text: &str) -> Result<jni::sys::jobject, Box<dyn StdError + Send + Sync>> {
        with_env_and_activity(|env, activity| {
            let ctx = activity.as_obj();
            let tv = env.new_object(
                "android/widget/TextView",
                "(Landroid/content/Context;)V",
                &[(&ctx).into()],
            )?;
            let j_text = env.new_string(text)?;
            env.call_method(
                &tv,
                "setText",
                "(Ljava/lang/CharSequence;)V",
                &[(&j_text).into()],
            )?;
            make_global_ref(env, &tv)
        })
    }

    pub fn create_box(orientation: i32, _spacing: i32) -> Result<jni::sys::jobject, Box<dyn StdError + Send + Sync>> {
        with_env_and_activity(|env, activity| {
            let ctx = activity.as_obj();
            let layout = env.new_object(
                "android/widget/LinearLayout",
                "(Landroid/content/Context;)V",
                &[(&ctx).into()],
            )?;
            env.call_method(
                &layout,
                "setOrientation",
                "(I)V",
                &[orientation.into()],
            )?;
            make_global_ref(env, &layout)
        })
    }

    pub fn create_entry() -> Result<jni::sys::jobject, Box<dyn StdError + Send + Sync>> {
        with_env_and_activity(|env, activity| {
            let ctx = activity.as_obj();
            let edit = env.new_object(
                "android/widget/EditText",
                "(Landroid/content/Context;)V",
                &[(&ctx).into()],
            )?;
            make_global_ref(env, &edit)
        })
    }

    pub fn create_grid() -> Result<jni::sys::jobject, Box<dyn StdError + Send + Sync>> {
        with_env_and_activity(|env, activity| {
            let ctx = activity.as_obj();
            let grid = env.new_object(
                "android/widget/GridLayout",
                "(Landroid/content/Context;)V",
                &[(&ctx).into()],
            )?;
            make_global_ref(env, &grid)
        })
    }

    pub fn create_dropdown(items: &[&str]) -> Result<jni::sys::jobject, Box<dyn StdError + Send + Sync>> {
        with_env_and_activity(|env, activity| {
            let ctx = activity.as_obj();
            let spinner = env.new_object(
                "android/widget/Spinner",
                "(Landroid/content/Context;)V",
                &[(&ctx).into()],
            )?;
            let array_cls = env.find_class("java/util/ArrayList")?;
            let list = env.new_object(
                &array_cls,
                "()V",
                &[],
            )?;
            for item in items {
                let j_item = env.new_string(item)?;
                env.call_method(
                    &list,
                    "add",
                    "(Ljava/lang/Object;)Z",
                    &[(&j_item).into()],
                )?;
            }
            let layout_id = &env.get_static_field(
                "android/R$layout",
                "simple_spinner_item",
                "I",
            )?.i()?;
            let adapter = env.new_object(
                "android/widget/ArrayAdapter",
                "(Landroid/content/Context;ILjava/util/List;)V",
                &[
                    (&ctx).into(),
                    (*layout_id).into(),
                    (&list).into(),
                ],
            )?;
            env.call_method(
                &spinner,
                "setAdapter",
                "(Landroid/widget/SpinnerAdapter;)V",
                &[(&adapter).into()],
            )?;
            make_global_ref(env, &spinner)
        })
    }

    pub fn create_checkbutton(label: &str) -> Result<jni::sys::jobject, Box<dyn StdError + Send + Sync>> {
        with_env_and_activity(|env, activity| {
            let ctx = activity.as_obj();
            let cb = env.new_object(
                "android/widget/CheckBox",
                "(Landroid/content/Context;)V",
                &[(&ctx).into()],
            )?;
            let j_label = env.new_string(label)?;
            env.call_method(
                &cb,
                "setText",
                "(Ljava/lang/CharSequence;)V",
                &[(&j_label).into()],
            )?;
            make_global_ref(env, &cb)
        })
    }

    pub fn create_radiobutton(group_ptr: *mut std::os::raw::c_void, label: &str) -> Result<jni::sys::jobject, Box<dyn StdError + Send + Sync>> {
        with_env_and_activity(|env, activity| {
            let ctx = activity.as_obj();
            let rb = env.new_object(
                "android/widget/RadioButton",
                "(Landroid/content/Context;)V",
                &[(&ctx).into()],
            )?;
            let j_label = env.new_string(label)?;
            env.call_method(
                &rb,
                "setText",
                "(Ljava/lang/CharSequence;)V",
                &[(&j_label).into()],
            )?;
            if !group_ptr.is_null() {
                let group_obj = unsafe { jni::objects::JObject::from_raw(group_ptr as jni::sys::jobject) };
                env.call_method(
                    &group_obj,
                    "addView",
                    "(Landroid/view/View;)V",
                    &[(&rb).into()],
                )?;
            }
            make_global_ref(env, &rb)
        })
    }

    pub fn create_radiogroup() -> Result<jni::sys::jobject, Box<dyn StdError + Send + Sync>> {
        with_env_and_activity(|env, activity| {
            let ctx = activity.as_obj();
            let rg = env.new_object(
                "android/widget/RadioGroup",
                "(Landroid/content/Context;)V",
                &[(&ctx).into()],
            )?;
            make_global_ref(env, &rg)
        })
    }

    pub fn create_dialog() -> Result<jni::sys::jobject, Box<dyn StdError + Send + Sync>> {
        with_env_and_activity(|env, activity| {
            let ctx = activity.as_obj();
            let builder = env.new_object(
                "android/app/AlertDialog$Builder",
                "(Landroid/content/Context;)V",
                &[(&ctx).into()],
            )?;
            make_global_ref(env, &builder)
        })
    }

    pub fn create_textview() -> Result<jni::sys::jobject, Box<dyn StdError + Send + Sync>> {
        with_env_and_activity(|env, activity| {
            let ctx = activity.as_obj();
            let tv = env.new_object(
                "android/widget/EditText",
                "(Landroid/content/Context;)V",
                &[(&ctx).into()],
            )?;
            let gravity = env.get_static_field(
                "android/view/Gravity",
                "TOP",
                "I",
            )?.i()?;
            env.call_method(
                &tv,
                "setGravity",
                "(I)V",
                &[gravity.into()],
            )?;
            let input_type = env.get_static_field(
                "android/text/InputType",
                "TYPE_TEXT_FLAG_MULTI_LINE",
                "I",
            )?.i()?;
            let class_type = env.get_static_field(
                "android/text/InputType",
                "TYPE_CLASS_TEXT",
                "I",
            )?.i()?;
            env.call_method(
                &tv,
                "setInputType",
                "(I)V",
                &[(class_type | input_type).into()],
            )?;
            env.call_method(
                &tv,
                "setMinLines",
                "(I)V",
                &[3i32.into()],
            )?;
            make_global_ref(env, &tv)
        })
    }

    /// Plain `android.view.View` backing a [`crate::backends_android_adapter::Canvas`].
    /// A custom `SheetView` (with `onDraw` funneling `DrawContext` calls back
    /// into Rust) replaces this once the Java side lands; until then a plain
    /// view keeps the widget tree valid so appends never see bogus handles.
    pub fn create_canvas_view() -> Result<(jni::sys::jobject, u64), Box<dyn StdError + Send + Sync>> {
        let class = SHEET_VIEW_CLASS.get().cloned().unwrap_or_else(|| "android/view/View".to_string());
        with_env_and_activity(|env, activity| {
            let ctx = activity.as_obj();
            // Custom SheetViews take (Context, long canvasId); the platform
            // View takes (Context). Try the two-arg form first, fall back.
            if class != "android/view/View" {
                let canvas_id = NEXT_CANVAS_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let resolved = load_app_class(env, &class).ok();
                if let Some(resolved) = resolved {
                    match env.new_object(
                        &resolved,
                        "(Landroid/content/Context;J)V",
                        &[(&ctx).into(), (canvas_id as i64).into()],
                    ) {
                        Ok(v) => {
                            let raw = make_global_ref(env, &v)?;
                            register_canvas_view(raw, canvas_id);
                            return Ok((raw, canvas_id));
                        }
                        Err(_) => {
                            let _ = env.exception_clear();
                        }
                    }
                }
            }
            let canvas_id = NEXT_CANVAS_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let view = env.new_object(
                "android/view/View",
                "(Landroid/content/Context;)V",
                &[(&ctx).into()],
            )?;
            let raw = make_global_ref(env, &view)?;
            register_canvas_view(raw, canvas_id);
            Ok((raw, canvas_id))
        })
    }

    static SHEET_VIEW_CLASS: OnceCell<String> = OnceCell::new();
    static NEXT_CANVAS_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

    /// View-pointer -> canvas-id map (the draw registry keys on canvas id;
    /// the Rust `Canvas` handle is the view pointer).
    static CANVAS_IDS: Lazy<Mutex<HashMap<usize, u64>>> = Lazy::new(|| Mutex::new(HashMap::new()));

    /// Views that asked to expand (`set_hexpand`). Android has no expand
    /// flag: the request is recorded here and honoured by
    /// `BoxWidget::append` as `LinearLayout` weight 1, so the formula entry
    /// grows instead of measuring to zero width.
    static EXPANDING: Lazy<Mutex<std::collections::HashSet<usize>>> =
        Lazy::new(|| Mutex::new(std::collections::HashSet::new()));

    /// Minimum content width in characters (GTK `set_width_chars`), kept so
    /// an empty EditText still gets a usable box.
    static MIN_CHARS: Lazy<Mutex<HashMap<usize, i32>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    pub fn set_view_expanding(view_ptr: *mut std::os::raw::c_void, expand: bool) {
        let mut set = EXPANDING.lock().unwrap();
        if expand {
            set.insert(view_ptr as usize);
        } else {
            set.remove(&(view_ptr as usize));
        }
    }

    pub fn is_view_expanding(view_ptr: *mut std::os::raw::c_void) -> bool {
        EXPANDING.lock().unwrap().contains(&(view_ptr as usize))
    }

    pub fn set_view_min_chars(view_ptr: *mut std::os::raw::c_void, n: i32) {
        MIN_CHARS.lock().unwrap().insert(view_ptr as usize, n);
    }

    pub fn view_min_chars(view_ptr: *mut std::os::raw::c_void) -> i32 {
        MIN_CHARS.lock().unwrap().get(&(view_ptr as usize)).copied().unwrap_or(0)
    }

    fn register_canvas_view(view_raw: jni::sys::jobject, canvas_id: u64) {
        CANVAS_IDS.lock().unwrap().insert(view_raw as usize, canvas_id);
    }

    /// Canvas id for a view pointer (0 = unknown).
    pub fn canvas_id_for_view(view_ptr: *mut std::os::raw::c_void) -> u64 {
        CANVAS_IDS.lock().unwrap().get(&(view_ptr as usize)).copied().unwrap_or(0)
    }

    /// True when `view_ptr` is a canvas view (SheetView or plain canvas):
    /// used for layout weighting (canvases grow, chrome wraps).
    pub fn is_canvas_view(view_ptr: *mut std::os::raw::c_void) -> bool {
        canvas_id_for_view(view_ptr) != 0
    }

    /// Register the fully-qualified custom View class used for canvases
    /// (e.g. `com.corro.SheetView`). Must have a `(Context, long)` ctor
    /// taking the Rust canvas id. Falls back to a plain View per canvas
    /// when unset or when instantiation fails.
    pub fn set_sheet_view_class(class: &str) {
        let _ = SHEET_VIEW_CLASS.set(class.to_string());
    }

    /// Install the shared `TextWatcher`/`OnEditorActionListener` on an
    /// EditText so text changes and IME "Done" reach Rust. The listeners are
    /// `com.corro.CorroTextWatcher` / `com.corro.CorroEditorAction` when
    /// present; absent classes make this a no-op (backend still works, just
    /// without the typing path).
    pub fn attach_text_watcher(entry_ptr: *mut std::os::raw::c_void) {
        let class_name = match ENTRY_LISTENER_CLASSES.get() {
            Some((watcher, _)) => watcher.clone(),
            None => "com/corro/CorroTextWatcher".to_string(),
        };
        attach_entry_listener(entry_ptr, "addTextChangedListener", &class_name);
    }

    /// See [`attach_text_watcher`]; wires `setOnEditorActionListener`.
    pub fn attach_editor_action(entry_ptr: *mut std::os::raw::c_void) {
        if entry_ptr.is_null() {
            return;
        }
        let (class_name, sig) = match ENTRY_LISTENER_CLASSES.get() {
            Some((_, editor)) => (editor.clone(), "(J)V".to_string()),
            None => ("com/corro/CorroEditorAction".to_string(), "(J)V".to_string()),
        };
        let _ = with_env_and_activity(|env, _activity| {
            let entry = unsafe { jni::objects::JObject::from_raw(entry_ptr as jni::sys::jobject) };
            let cls = match load_app_class(env, &class_name) {
                Ok(c) => c,
                Err(_) => {
                    let _ = env.exception_clear();
                    return Ok::<_, Box<dyn StdError + Send + Sync>>(());
                }
            };
            let listener = env.new_object(&cls, &sig, &[(entry_ptr as i64).into()])?;
            crate::backends::android::logcat_rs("editor-action listener created");
            env.call_method(
                &entry,
                "setOnEditorActionListener",
                "(Landroid/widget/TextView$OnEditorActionListener;)V",
                &[(&listener).into()],
            )?;
            // Hardware/adb-injected Enter arrives as a raw key event, never
            // as an editor action: attach the key fallback too. Same class
            // loader, same native callback.
            let key_cls = load_app_class(env, "com/corro/CorroKeyListener").ok();
            if let Some(key_cls) = key_cls {
                crate::backends::android::logcat_rs("key-listener class resolved");
                if let Ok(key_listener) =
                    env.new_object(&key_cls, "(J)V", &[(entry_ptr as i64).into()])
                {
                    let _ = env.call_method(
                        &entry,
                        "setOnKeyListener",
                        "(Landroid/view/View$OnKeyListener;)V",
                        &[(&key_listener).into()],
                    );
                    crate::backends::android::logcat_rs("key-listener attached");
                } else {
                    let _ = env.exception_clear();
                    crate::backends::android::logcat_rs("key-listener new_object FAILED");
                }
            } else {
                crate::backends::android::logcat_rs("key-listener class NOT FOUND");
            }
            Ok::<_, Box<dyn StdError + Send + Sync>>(())
        });
    }

    fn attach_entry_listener(
        entry_ptr: *mut std::os::raw::c_void,
        method: &str,
        class_name: &str,
    ) {
        if entry_ptr.is_null() {
            return;
        }
        let _ = with_env_and_activity(|env, _activity| {
            let entry = unsafe { jni::objects::JObject::from_raw(entry_ptr as jni::sys::jobject) };
            let cls = match load_app_class(env, class_name) {
                Ok(c) => c,
                Err(_) => {
                    let _ = env.exception_clear();
                    return Ok::<_, Box<dyn StdError + Send + Sync>>(());
                }
            };
            let listener = env.new_object(&cls, "(J)V", &[(entry_ptr as i64).into()])?;
            env.call_method(
                &entry,
                method,
                "(Landroid/text/TextWatcher;)V",
                &[(&listener).into()],
            )?;
            Ok::<_, Box<dyn StdError + Send + Sync>>(())
        });
    }

    /// Override the entry listener class names (tests/alternative apps):
    /// (text-watcher class, editor-action class).
    static ENTRY_LISTENER_CLASSES: OnceCell<(String, String)> = OnceCell::new();

    pub fn set_entry_listener_classes(watcher: &str, editor_action: &str) {
        let _ = ENTRY_LISTENER_CLASSES.set((watcher.to_string(), editor_action.to_string()));
    }

    /// `View.invalidate()` on a canvas handle: schedules `onDraw`, which
    /// funnels back into the registered Rust draw closure. Best-effort.
    pub fn invalidate_view(view_ptr: *mut std::os::raw::c_void) {
        if view_ptr.is_null() {
            return;
        }
        let _ = with_env_and_activity(|env, _activity| {
            let view = unsafe { jni::objects::JObject::from_raw(view_ptr as jni::sys::jobject) };
            env.call_method(&view, "invalidate", "()V", &[])?;
            Ok::<_, Box<dyn StdError + Send + Sync>>(())
        });
    }

    /// Attach `child` to a container view (`FrameLayout` scrolled windows,
    /// overlays): `addView(child)`. Best-effort; ignores null handles.
    /// Insert `child` at `index` in a container (`addView(child, index)`),
    /// so a caller can put chrome above children that were added earlier.
    /// Best-effort; ignores null handles.
    pub fn insert_child_at(
        container_ptr: *mut std::os::raw::c_void,
        child_ptr: *mut std::os::raw::c_void,
        index: i32,
    ) {
        if container_ptr.is_null() || child_ptr.is_null() {
            return;
        }
        let _ = with_env_and_activity(|env, _activity| {
            let container =
                unsafe { jni::objects::JObject::from_raw(container_ptr as jni::sys::jobject) };
            let child =
                unsafe { jni::objects::JObject::from_raw(child_ptr as jni::sys::jobject) };
            env.call_method(
                &container,
                "addView",
                "(Landroid/view/View;I)V",
                &[(&child).into(), index.into()],
            )?;
            Ok::<_, Box<dyn StdError + Send + Sync>>(())
        });
    }

    /// Insert `child` at index 0 of `container` with a pinned layout:
    /// `WRAP_CONTENT` height and zero weight, so a `LinearLayout` sibling that
    /// carries weight 1 (the sheet) expands *below* it instead of competing
    /// with it. Without the explicit params the child inherits default
    /// `LayoutParams`, which lets the weighted sheet squeeze the strip.
    /// Best-effort; ignores null handles.
    pub fn pin_child_at_top(
        container_ptr: *mut std::os::raw::c_void,
        child_ptr: *mut std::os::raw::c_void,
    ) {
        if container_ptr.is_null() || child_ptr.is_null() {
            return;
        }
        let _ = with_env_and_activity(|env, _activity| {
            let container =
                unsafe { jni::objects::JObject::from_raw(container_ptr as jni::sys::jobject) };
            let child =
                unsafe { jni::objects::JObject::from_raw(child_ptr as jni::sys::jobject) };
            // MATCH_PARENT width (-1), WRAP_CONTENT height (-2), weight 0.
            let params = env.new_object(
                "android/widget/LinearLayout$LayoutParams",
                "(IIF)V",
                &[(-1i32).into(), (-2i32).into(), 0.0f32.into()],
            )?;
            env.call_method(
                &container,
                "addView",
                "(Landroid/view/View;ILandroid/view/ViewGroup$LayoutParams;)V",
                &[(&child).into(), 0i32.into(), (&params).into()],
            )?;
            Ok::<_, Box<dyn StdError + Send + Sync>>(())
        });
    }

    /// Call a no-argument `void` method on a view handle, swallowing failure.
    ///
    /// The raw `jobject` handles are kept alive by [`KEEP_ALIVE`], and the
    /// JVM is attached by [`with_env_and_activity`], so reconstructing the
    /// `JObject` for the duration of one call is the pattern every method
    /// here uses. This is the short form of it, for the many
    /// `setSomething(boolean)` / `setSomething(int)` view properties that
    /// have no business being a dozen lines of JNI each.
    fn call_view_method(
        view_ptr: *mut std::os::raw::c_void,
        name: &str,
        sig: &str,
        args: &[jni::objects::JValue<'_, '_>],
    ) -> Result<(), Box<dyn StdError + Send + Sync>> {
        if view_ptr.is_null() {
            return Ok(());
        }
        with_env_and_activity(|env, _activity| {
            let view = unsafe { jni::objects::JObject::from_raw(view_ptr as jni::sys::jobject) };
            env.call_method(&view, name, sig, args)?;
            Ok(())
        })
    }

    /// `View.setVisibility`, with `visible` mapped to the platform constants
    /// (`VISIBLE`/`INVISIBLE`/`GONE`). Used by every `set_visible`, so the
    /// choice of `INVISIBLE` over `GONE` is made in one place.
    pub fn set_view_visible(view_ptr: *mut std::os::raw::c_void, visible: bool) {
        // View.VISIBLE = 0, View.INVISIBLE = 4.
        let _ = call_view_method(
            view_ptr,
            "setVisibility",
            "(I)V",
            &[if visible { 0i32.into() } else { 4i32.into() }],
        );
    }

    /// `View.setMinimumWidth` / `setMinimumHeight`, clamped the way GTK
    /// clamps a `-1` size request back to the natural size. A caller asking
    /// for "no minimum" passes a negative value, which is a no-op here rather
    /// than a minimum of `-1` pixels.
    pub fn set_view_min_size(view_ptr: *mut std::os::raw::c_void, w: i32, h: i32) {
        if w > 0 {
            let _ = call_view_method(
                view_ptr,
                "setMinimumWidth",
                "(I)V",
                &[w.into()],
            );
        }
        if h > 0 {
            let _ = call_view_method(
                view_ptr,
                "setMinimumHeight",
                "(I)V",
                &[h.into()],
            );
        }
    }

    /// `View.setLayoutParams` with explicit width/height/weight, creating a
    /// `LinearLayout.LayoutParams` first.
    ///
    /// `MATCH_PARENT = -1`, `WRAP_CONTENT = -2`, so the caller passes the raw
    /// Android constants and this function only supplies the weight field,
    /// which is what expansion means in a `LinearLayout`.
    pub fn set_view_layout(
        view_ptr: *mut std::os::raw::c_void,
        width: i32,
        height: i32,
        weight: f32,
    ) {
        if view_ptr.is_null() {
            return;
        }
        let _ = with_env_and_activity(|env, _activity| {
            let params = env.new_object(
                "android/widget/LinearLayout$LayoutParams",
                "(IIF)V",
                &[width.into(), height.into(), weight.into()],
            )?;
            let view = unsafe { jni::objects::JObject::from_raw(view_ptr as jni::sys::jobject) };
            env.call_method(
                &view,
                "setLayoutParams",
                "(Landroid/view/ViewGroup$LayoutParams;)V",
                &[(&params).into()],
            )?;
            Ok::<_, Box<dyn StdError + Send + Sync>>(())
        });
    }

    /// `View.setPadding(left, top, right, bottom)`.
    pub fn set_view_padding(
        view_ptr: *mut std::os::raw::c_void,
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    ) {
        let _ = call_view_method(
            view_ptr,
            "setPadding",
            "(IIII)V",
            &[left.into(), top.into(), right.into(), bottom.into()],
        );
    }

    /// `View.setFocusable` + `setFocusableInTouchMode`, then `requestFocus`.
    ///
    /// Both flags are needed: a `View` in a touch-mode activity is not
    /// focusable by default, and a `requestFocus` on a view that is not
    /// focusable in touch mode is a silent no-op — which is exactly the bug
    /// that would make the formula bar unreachable by tapping it.
    pub fn focus_view(view_ptr: *mut std::os::raw::c_void) {
        let _ = call_view_method(view_ptr, "setFocusable", "(Z)V", &[true.into()]);
        let _ = call_view_method(
            view_ptr,
            "setFocusableInTouchMode",
            "(Z)V",
            &[true.into()],
        );
        let _ = call_view_method(view_ptr, "requestFocus", "()Z", &[]);
    }

    /// `View.setGravity`, from a raw bitmask. The callers in the adapter
    /// resolve the constant (`Gravity.START`, `CENTER`, ...) through JNI once.
    pub fn set_view_gravity(view_ptr: *mut std::os::raw::c_void, gravity: i32) {
        let _ = call_view_method(view_ptr, "setGravity", "(I)V", &[gravity.into()]);
    }

    /// The view's current gravity bitmask, or 0 when it cannot be read.
    pub fn get_view_gravity(view_ptr: *mut std::os::raw::c_void) -> i32 {
        if view_ptr.is_null() {
            return 0;
        }
        with_env_and_activity(|env, _activity| {
            let view = unsafe { jni::objects::JObject::from_raw(view_ptr as jni::sys::jobject) };
            Ok::<i32, Box<dyn StdError + Send + Sync>>(
                env.call_method(&view, "getGravity", "()I", &[])?.i()?,
            )
        })
        .unwrap_or(0)
    }

    /// Install a global-layout listener on a view.
    ///
    /// `OnGlobalLayoutListener` is the platform's "the bounds settled" hook —
    /// the moment GTK's size-allocate fires and the moment a caller needs to
    /// read a real width or height. The listener object carries the *view*
    /// pointer, so `dispatch_layout` can find the registered callback.
    pub fn attach_layout_listener(view_ptr: *mut std::os::raw::c_void) {
        if view_ptr.is_null() {
            return;
        }
        let _ = with_env_and_activity(|env, _activity| {
            let cls = match load_app_class(env, "com.corro.CorroLayout") {
                Ok(c) => c,
                Err(_) => {
                    let _ = env.exception_clear();
                    return Ok(());
                }
            };
            let listener = env.new_object(&cls, "(J)V", &[(view_ptr as i64).into()])?;
            PLATFORM_LISTENERS
                .lock()
                .unwrap()
                .push(env.new_global_ref(&listener)?);
            let view = unsafe { jni::objects::JObject::from_raw(view_ptr as jni::sys::jobject) };
            // `View` has no `setOnGlobalLayoutListener`: the listener is
            // registered on the *observer*, and the observer is replaced
            // whenever the view is detached, so it is re-fetched here rather
            // than cached.
            let observer = env
                .call_method(
                    &view,
                    "getViewTreeObserver",
                    "()Landroid/view/ViewTreeObserver;",
                    &[],
                )?
                .l()?;
            let observer = unsafe { jni::objects::JObject::from_raw(observer.as_raw()) };
            env.call_method(
                &observer,
                "addOnGlobalLayoutListener",
                "(Landroid/view/ViewTreeObserver$OnGlobalLayoutListener;)V",
                &[(&listener).into()],
            )?;
            Ok::<(), Box<dyn StdError + Send + Sync>>(())
        });
    }

    /// Read a `TextView`-shaped string property (`getText().toString()`).
    /// Shared by `Label::get_text` and the entry/buffer getters.
    pub fn get_view_text(view_ptr: *mut std::os::raw::c_void) -> Option<String> {
        if view_ptr.is_null() {
            return None;
        }
        with_env_and_activity(|env, _activity| {
            let view = unsafe { jni::objects::JObject::from_raw(view_ptr as jni::sys::jobject) };
            let value = env.call_method(&view, "getText", "()Ljava/lang/CharSequence;", &[])?;
            let obj = value.l()?;
            let obj = unsafe { jni::objects::JObject::from_raw(obj.as_raw()) };
            let jstr: JString = JString::from(obj);
            let text: String = env.get_string(&jstr)?.into();
            Ok(text)
        })
        .ok()
    }

    /// `View.setTextSize(complexSizeP)` with a sp size, so a label's font
    /// scales with the user's font-size preference instead of being pinned in
    /// pixels. `sp` is the size in scale-independent pixels.
    pub fn set_view_text_size_sp(view_ptr: *mut std::os::raw::c_void, sp: f32) {
        let _ = call_view_method(view_ptr, "setTextSize", "(F)V", &[sp.into()]);
    }

    /// `Typeface` style from a GTK/NWG `font-style`-shaped integer: 0 normal,
    /// 1 bold, 2 italic, 3 bold-italic.
    ///
    /// The pass-through is deliberate: GTK's `PANGO_STYLE_BOLD` is 1 and
    /// `PANGO_STYLE_ITALIC` is 2, which is exactly the bit layout Android's
    /// `Typeface` constants use (`BOLD` = 1, `ITALIC` = 2,
    /// `BOLD_ITALIC` = 3). Mapping "any non-zero to bold" — the obvious
    /// shortcut — would render an italic label bold and lose the distinction
    /// the caller asked for.
    pub fn set_view_typeface(view_ptr: *mut std::os::raw::c_void, style: i32) {
        if view_ptr.is_null() {
            return;
        }
        let want = style.clamp(0, 3);
        let _ = with_env_and_activity(|env, _activity| {
            let typeface_cls = env.find_class("android/graphics/Typeface")?;
            let base = env
                .get_static_field(&typeface_cls, "DEFAULT", "Landroid/graphics/Typeface;")?
                .l()?;
            let base = unsafe { jni::objects::JObject::from_raw(base.as_raw()) };
            // Typeface.create(Typeface, int style) -> Typeface
            let styled = env
                .call_static_method(
                    &typeface_cls,
                    "create",
                    "(Landroid/graphics/Typeface;I)Landroid/graphics/Typeface;",
                    &[(&base).into(), want.into()],
                )?
                .l()?;
            let styled = unsafe { jni::objects::JObject::from_raw(styled.as_raw()) };
            let view = unsafe { jni::objects::JObject::from_raw(view_ptr as jni::sys::jobject) };
            env.call_method(
                &view,
                "setTypeface",
                "(Landroid/graphics/Typeface;)V",
                &[(&styled).into()],
            )?;
            Ok::<(), Box<dyn StdError + Send + Sync>>(())
        });
    }

    // ---- CSS class bridge ----
    //
    // Android has no stylesheet cascade, so `add_class` / `remove_class`
    // cannot change a pixel by themselves. They are recorded here instead of
    // dropped, and the list is readable from Java, which is what lets a host
    // install an optional bridge that maps classes to background colour /
    // text colour. Recording is the point: a caller that asks to dim a
    // read-only sheet gets a queryable fact instead of silence.

    static VIEW_CLASSES: Lazy<Mutex<HashMap<usize, Vec<String>>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    pub fn add_view_class(view_ptr: *mut std::os::raw::c_void, class_name: &str) {
        if view_ptr.is_null() || class_name.is_empty() {
            return;
        }
        VIEW_CLASSES
            .lock()
            .unwrap()
            .entry(view_ptr as usize)
            .or_default()
            .push(class_name.to_string());
    }

    pub fn remove_view_class(view_ptr: *mut std::os::raw::c_void, class_name: &str) {
        if let Some(list) = VIEW_CLASSES.lock().unwrap().get_mut(&(view_ptr as usize)) {
            list.retain(|c| c != class_name);
        }
    }

    /// The classes on a view, in the order they were added. Empty when none.
    /// Exposed for the host bridge and for diagnostics.
    pub fn view_classes(view_ptr: *mut std::os::raw::c_void) -> Vec<String> {
        VIEW_CLASSES
            .lock()
            .unwrap()
            .get(&(view_ptr as usize))
            .cloned()
            .unwrap_or_default()
    }

    /// `View.getContext()` -> `Context`, as a local ref for the caller.
    /// Used where a new object needs the Activity context but the caller
    /// already holds a view handle (dialogs, popups).
    pub fn with_view_context<F, T>(view_ptr: *mut std::os::raw::c_void, f: F) -> Option<T>
    where
        F: FnOnce(&mut JNIEnv<'_>, &JObject<'_>) -> T,
    {
        if view_ptr.is_null() {
            return None;
        }
        with_env_and_activity(|env, _activity| {
            let view = unsafe { jni::objects::JObject::from_raw(view_ptr as jni::sys::jobject) };
            let ctx = env
                .call_method(&view, "getContext", "()Landroid/content/Context;", &[])?
                .l()?;
            let ctx = unsafe { jni::objects::JObject::from_raw(ctx.as_raw()) };
            Ok(f(env, &ctx))
        })
        .ok()
    }

    /// `Handler.postDelayed` timers, the Android counterpart of GTK's
    /// `timeout_add` and NWG's message timers.
    ///
    /// There is no event loop to run a source on, so a timer is a
    /// `Handler` posting a `Runnable`. The `Runnable` is the host's
    /// `com.corro.RustCallback`-shaped class resolved through the app
    /// ClassLoader and handed the timer id; it calls back into
    /// [`dispatch_timeout`]. A repeating timer re-posts itself there, which
    /// is why `repeat` lives in the registry rather than in Java.
    ///
    /// Each timer also holds a `Runnable` handle so [`cancel_timeout`] can
    /// `removeCallbacks` it: `removeCallbacksAndMessages` on a token is the
    /// only way to cancel one repeating post without cancelling the others.
    static TIMERS: Lazy<Mutex<HashMap<u64, TimerEntry>>> = Lazy::new(|| Mutex::new(HashMap::new()));
    static NEXT_TIMER_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

    struct TimerEntry {
        delay_ms: u32,
        f: extern "C" fn(),
        repeat: bool,
    }

    /// Create (or look up) the host's runnable class. `com.corro.CorroTimer`
    /// takes the timer id as its only constructor argument, exactly like
    /// `RustCallback`; a host that does not ship one gets a silent no-op
    /// timer, so the rest of the UI still builds.
    fn timer_runnable<'a>(
        env: &mut JNIEnv<'a>,
        id: u64,
    ) -> Result<JObject<'a>, Box<dyn StdError + Send + Sync>> {
        let cls = match load_app_class(env, "com.corro.CorroTimer") {
            Ok(c) => c,
            Err(_) => {
                let _ = env.exception_clear();
                return Err("com.corro.CorroTimer not found".into());
            }
        };
        Ok(env.new_object(&cls, "(J)V", &[(id as i64).into()])?.into())
    }

    /// Register a timer and post its first run. Returns the timer id, or 0
    /// when the host supplies no timer runnable class.
    pub fn schedule_timeout(
        delay_ms: u32,
        f: extern "C" fn(),
        repeat: bool,
    ) -> u64 {
        let Ok((id, runnable)) = with_env_and_activity(|env, activity| {
            let id = NEXT_TIMER_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            TIMERS.lock().unwrap().insert(
                id,
                TimerEntry {
                    delay_ms,
                    f,
                    repeat,
                },
            );
            let runnable = timer_runnable(env, id)?;
            // Handler() uses the creating thread's Looper. The UI thread is
            // the one that built the widget tree, so this is the UI looper.
            let handler = env.new_object("android/os/Handler", "()V", &[])?;
            let posted = env.call_method(
                &handler,
                "postDelayed",
                "(Ljava/lang/Runnable;J)Z",
                &[(&runnable).into(), (delay_ms as i64).into()],
            )?;
            // Keep the Handler reachable for cancellation; without it the
            // only handle on the post is the id.
            HANDLERS.lock().unwrap().insert(id, env.new_global_ref(&handler)?);
            let _ = activity;
            Ok((id, posted.i()?))
        }) else {
            return 0;
        };
        if runnable == 0 {
            TIMERS.lock().unwrap().remove(&id);
            HANDLERS.lock().unwrap().remove(&id);
            return 0;
        }
        logcat_rs(&format!("timer {id} armed ({delay_ms}ms, repeat={repeat})"));
        id
    }

    /// Timer `Runnable` -> Handler, for `cancel_timeout`.
    static HANDLERS: Lazy<Mutex<HashMap<u64, GlobalRef>>> = Lazy::new(|| Mutex::new(HashMap::new()));

    /// Cancel a timer and free its `Handler`. Safe to call twice.
    pub fn cancel_timeout(id: u64) {
        let Some(handler) = HANDLERS.lock().unwrap().remove(&id) else {
            return;
        };
        let _ = with_env_and_activity(|env, _activity| {
            // postAtTime with no runnable is the documented way to drop a
            // single pending post without touching the rest of the queue.
            env.call_method(
                handler.as_obj(),
                "removeCallbacks",
                "(Ljava/lang/Runnable;)V",
                &[],
            )?;
            Ok::<_, Box<dyn StdError + Send + Sync>>(())
        });
        TIMERS.lock().unwrap().remove(&id);
    }

    /// Called from the host's timer `Runnable` (on the UI thread). Runs the
    /// registered function and re-posts a repeating timer.
    ///
    /// The entry is taken out of the map *before* the call so a function
    /// that cancels itself does not deadlock on the registry, and so a
    /// function that re-arms registers a fresh id rather than racing the
    /// re-post below.
    pub fn dispatch_timeout(id: u64) {
        let Some(entry) = TIMERS.lock().unwrap().remove(&id) else {
            return;
        };
        (entry.f)();
        if !entry.repeat {
            HANDLERS.lock().unwrap().remove(&id);
            return;
        }
        let _ = with_env_and_activity(|env, _activity| {
            let runnable = timer_runnable(env, id)?;
            let Some(handler) = HANDLERS.lock().unwrap().get(&id).cloned() else {
                let _ = env.exception_clear();
                return Ok(());
            };
            env.call_method(
                handler.as_obj(),
                "postDelayed",
                "(Ljava/lang/Runnable;J)Z",
                &[(&runnable).into(), (entry.delay_ms as i64).into()],
            )?;
            TIMERS.lock().unwrap().insert(
                id,
                TimerEntry {
                    delay_ms: entry.delay_ms,
                    f: entry.f,
                    repeat: true,
                },
            );
            Ok::<(), Box<dyn StdError + Send + Sync>>(())
        });
    }

    // ---- View measurement / selection / focus queries ----

    /// `EditText.getSelectionStart()` as a character index.
    ///
    /// A *selection range* collapses to its start, which is the convention GTK
    /// and NWG use: both report the mark, and a caller that wants the extent
    /// asks for the end separately.
    pub fn get_view_selection_start(view_ptr: *mut std::os::raw::c_void) -> Option<usize> {
        if view_ptr.is_null() {
            return None;
        }
        with_env_and_activity(|env, _activity| {
            let view = unsafe { jni::objects::JObject::from_raw(view_ptr as jni::sys::jobject) };
            let start = env
                .call_method(&view, "getSelectionStart", "()I", &[])?
                .i()?;
            if start < 0 {
                // -1 means "no selection", which is what an EditText reports
                // when it has never been focused. Treat it as the start of the
                // text rather than as "cannot answer", so a caller that
                // probes before focusing gets 0 instead of None.
                return Ok(0usize);
            }
            Ok(start as usize)
        })
        .ok()
    }

    /// Character length of a `TextView`'s text, read through JNI.
    fn view_text_len(env: &mut JNIEnv<'_>, view: &JObject<'_>) -> Result<usize, Box<dyn StdError + Send + Sync>> {
        let value = env.call_method(view, "getText", "()Ljava/lang/CharSequence;", &[])?;
        let obj = value.l()?;
        let jstr = JString::from(obj);
        let text: String = env.get_string(&jstr)?.into();
        Ok(text.chars().count())
    }

    /// Move the selection to a character index, clamped to the text length.
    /// A `start == end` selection is the caret.
    pub fn set_view_selection(view_ptr: *mut std::os::raw::c_void, pos: usize) {
        if view_ptr.is_null() {
            return;
        }
        let _ = with_env_and_activity(|env, _activity| {
            let view = unsafe { jni::objects::JObject::from_raw(view_ptr as jni::sys::jobject) };
            // Clamp against the real text length, so a caller that computed a
            // stale index cannot make `setSelection` throw and leave the
            // caret where it was.
            let text = view_text_len(env, &view)?;
            let clamped = pos.min(text) as i32;
            env.call_method(&view, "setSelection", "(II)V", &[clamped.into(), clamped.into()])?;
            Ok::<(), Box<dyn StdError + Send + Sync>>(())
        });
    }

    /// The view's top-left in *screen* coordinates, from
    /// `getLocationOnScreen`.
    ///
    /// Not the same as the view's position in its parent: the sheet is nested
    /// under the Activity's root beneath a pinned menu strip, so a caller
    /// popping a menu at a guessed position puts it off by the strip's height.
    pub fn get_view_screen_origin(view_ptr: *mut std::os::raw::c_void) -> Option<(i32, i32)> {
        if view_ptr.is_null() {
            return None;
        }
        with_env_and_activity(|env, _activity| {
            let view = unsafe { jni::objects::JObject::from_raw(view_ptr as jni::sys::jobject) };
            // getLocationOnScreen fills an int[2]; it is an out-param, not a
            // return value, so the array is allocated here and read back.
            let out = env.new_int_array(2)?;
            env.call_method(&view, "getLocationOnScreen", "([I)V", &[(&out).into()])?;
            let mut buf = [0i32; 2];
            env.get_int_array_region(&out, 0, &mut buf)?;
            Ok::<(i32, i32), Box<dyn StdError + Send + Sync>>((buf[0], buf[1]))
        })
        .ok()
    }

    // ---- ScrolledWindow: a real ScrollView ----

    /// `ScrollView.setHorizontalScrollBarEnabled` / `setVerticalScrollBarEnabled`.
    ///
    /// The policy constants are the shared backend-agnostic ones GTK's
    /// `set_policy` and NWG's use: 0 never, 1 always, 2 automatic. Any other
    /// value falls back to automatic, which is Android's default and the safe
    /// answer for a value this backend did not define.
    pub fn set_scrolled_policy(view_ptr: *mut std::os::raw::c_void, h: u32, v: u32) {
        if view_ptr.is_null() {
            return;
        }
        let _ = with_env_and_activity(|env, _activity| {
            let view = unsafe { jni::objects::JObject::from_raw(view_ptr as jni::sys::jobject) };
            // 1 = always on, 0 = off. `never` and `automatic` both mean "the
            // platform decides", which is `setEnabled(false)` for a
            // never-policy and leave-it-alone otherwise... but a never-policy
            // must actively hide the bar, so: always -> true, everything
            // else -> false is wrong for automatic. The distinction that
            // matters to a caller is "on" vs "off"; automatic is the
            // platform default, which is *on but only when needed*, so it is
            // expressed as enabled.
            let horiz = h != 0;
            let vert = v != 0;
            env.call_method(
                &view,
                "setHorizontalScrollBarEnabled",
                "(Z)V",
                &[horiz.into()],
            )?;
            env.call_method(&view, "setVerticalScrollBarEnabled", "(Z)V", &[vert.into()])?;
            Ok::<(), Box<dyn StdError + Send + Sync>>(())
        });
    }

    /// Scroll a `ScrollView` to a position expressed in the backend-agnostic
    /// *item* model (`value` of `upper`, page size `page`).
    ///
    /// The platform's `scrollTo` is in pixels, so the item index is turned
    /// into a fraction of the scrollable extent. `upper <= page` means
    /// everything fits and there is nothing to scroll to, so it is 0 rather
    /// than a division by zero — that check is the entire reason this
    /// function does not just call `scrollTo(0, ratio * height)`.
    pub fn scroll_scrolled_window(
        view_ptr: *mut std::os::raw::c_void,
        hval: f64,
        hupper: f64,
        hpage: f64,
        vval: f64,
        vupper: f64,
        vpage: f64,
    ) {
        if view_ptr.is_null() {
            return;
        }
        let _ = with_env_and_activity(|env, _activity| {
            let view = unsafe { jni::objects::JObject::from_raw(view_ptr as jni::sys::jobject) };
            let child = env
                .call_method(&view, "getChildAt", "(I)Landroid/view/View;", &[0i32.into()])?
                .l()?;
            if child.is_null() {
                return Ok(());
            }
            let child = unsafe { jni::objects::JObject::from_raw(child.as_raw()) };
            let w = env.call_method(&child, "getWidth", "()I", &[])?.i()?;
            let h = env.call_method(&child, "getHeight", "()I", &[])?.i()?;
            let frac = |val: f64, upper: f64, page: f64| -> f64 {
                let span = upper - page;
                if !span.is_finite() || span <= 0.0 {
                    0.0
                } else {
                    (val / span).clamp(0.0, 1.0)
                }
            };
            let x = (frac(hval, hupper, hpage) * w.max(0) as f64) as i32;
            let y = (frac(vval, vupper, vpage) * h.max(0) as f64) as i32;
            // smoothScrollTo animates, which is what a scrollbar drag wants
            // and what scroll_to from a program *usually* wants on the
            // desktop backends too.
            env.call_method(
                &view,
                "smoothScrollTo",
                "(II)V",
                &[x.into(), y.into()],
            )?;
            Ok::<(), Box<dyn StdError + Send + Sync>>(())
        });
    }

    // ---- Key translation ----

    /// Android `KeyEvent` constants, named. These are stable ints from API 1,
    /// so naming them here (rather than resolving each through a JNI static
    /// lookup) keeps the translation table readable and cheap. The values
    /// are the AOSP `KeyEvent.KEYCODE_*` assignments.
    mod keycode {
        pub const DPAD_UP: i32 = 19;
        pub const DPAD_DOWN: i32 = 20;
        pub const DPAD_LEFT: i32 = 21;
        pub const DPAD_RIGHT: i32 = 22;
        pub const TAB: i32 = 61;
        pub const ENTER: i32 = 66;
        pub const DEL: i32 = 67;
        pub const FORWARD_DEL: i32 = 112;
        pub const ESCAPE: i32 = 111;
        pub const MOVE_HOME: i32 = 122;
        pub const MOVE_END: i32 = 123;
        pub const PAGE_UP: i32 = 92;
        pub const PAGE_DOWN: i32 = 93;
        pub const F1: i32 = 131;
        pub const F12: i32 = 142;
    }

    /// Translate an Android keycode to the GDK/X11 keysym
    /// `rswidgets::core::key` uses.
    ///
    /// Returns `None` for a key with no counterpart, which the caller reports
    /// as "not ours" so the platform handles it. That distinction matters:
    /// answering `0` for an unknown key would make `handle_key` treat it as
    /// a printable NUL and start an edit.
    ///
    /// The named keysyms are the *unix* set, which is what `core::key` defines
    /// for every non-Windows target. They are spelled as literals because
    /// this module is `#[cfg(target_os = "android")]` inside a file whose
    /// `core::key` re-export is already resolved by the adapter, and because
    /// a unit test below asserts each literal against the real constant so
    /// the two cannot drift.
    pub fn android_keycode_to_gdk(keycode: i32) -> Option<u32> {
        use keycode::*;
        Some(match keycode {
            DPAD_LEFT => 0xFF51,  // LEFT
            DPAD_UP => 0xFF52,    // UP
            DPAD_RIGHT => 0xFF53, // RIGHT
            DPAD_DOWN => 0xFF54,  // DOWN
            TAB => 0xFF09,        // TAB
            ENTER => 0xFF0D,      // RETURN
            DEL => 0xFF08,        // BACKSPACE
            FORWARD_DEL => 0xFFFF,// DELETE
            ESCAPE => 0xFF1B,     // ESCAPE
            MOVE_HOME => 0xFF50,  // HOME
            MOVE_END => 0xFF57,   // END
            PAGE_UP => 0xFF55,    // PAGE_UP
            PAGE_DOWN => 0xFF56,  // PAGE_DOWN
            F1..=F12 => {
                // F1..F12 are contiguous (131..142) and GDK's are
                // (0xFFBE..0xFFC9), so one offset covers the range instead of
                // twelve arms that would each have to be edited together.
                0xFFBE + (keycode - F1) as u32
            }
            _ => return None,
        })
    }

    /// Modifier bits as `core::key`'s callers spell them, which is the GTK
    /// `GdkModifierType` layout: 0x1 Shift, 0x2 Lock, 0x4 Control, 0x8 Mod1.
    ///
    /// The Android mapping: `KEYCODE_SHIFT_LEFT/RIGHT` set Shift, and
    /// `KEYCODE_ALT_LEFT/RIGHT` set **Mod1** (not "Alt"), because the bit
    /// 0x8 position is what `gui_backend`'s `alt_held` test reads. A
    /// `MetaEvent.getModifierState` mask is the authoritative source for
    /// software modifiers, because a hardware key's own modifier state is
    /// often empty at `onKeyDown` time; the two are OR-ed so a real key press
    /// is never lost when the meta state lags.
    pub fn android_key_mods(key_code: i32, meta_mask: i32) -> u32 {
        // KeyEvent.META_SHIFT_ON = 0x1, META_CTRL_ON = 0x1000,
        // META_ALT_ON = 0x2000, META_META_ON = 0x10000.
        const META_SHIFT_ON: i32 = 0x0000_0001;
        const META_CTRL_ON: i32 = 0x0000_1000;
        const META_ALT_ON: i32 = 0x0000_2000;
        const META_META_ON: i32 = 0x0001_0000;

        let mut mods = 0u32;
        if meta_mask & META_SHIFT_ON != 0 {
            mods |= 0x1;
        }
        if meta_mask & META_CTRL_ON != 0 {
            mods |= 0x4;
        }
        // Alt and Meta both map onto Mod1 (bit 0x8): a desktop spreadsheet
        // treats Alt and the Windows key identically for menu mnemonics, and
        // the emulator reports a "Meta" key where a desktop build would have
        // Alt. Merging is what makes Ctrl+Alt+... and the emulator's Meta
        // shortcuts reach the same code.
        if meta_mask & (META_ALT_ON | META_META_ON) != 0 {
            mods |= 0x8;
        }
        // The physical modifier key itself. A modifier's own press arrives
        // with an empty meta state on some IMEs, so the key code is OR-ed in
        // rather than preferred: losing Shift there would silently turn
        // Shift+arrows into plain moves, which is the difference between
        // moving the cursor and extending the selection.
        match key_code {
            59 | 60 => mods |= 0x1,          // SHIFT_LEFT / SHIFT_RIGHT
            57 | 58 => mods |= 0x4,          // CTRL_LEFT / CTRL_RIGHT
            62 | 63 => mods |= 0x8,          // ALT_LEFT / ALT_RIGHT
            91 | 92 | 93 => mods |= 0x8,     // META_LEFT / _RIGHT / META_STAR
            _ => {}
        }
        mods
    }

    // ---- Platform listener attachment ----
    //
    // JNI cannot create a proxy for a Java interface, so each Android
    // listener needs a concrete host class. Each takes the id the callback
    // registry keyed on and forwards; all of them are optional, and a host
    // that ships none gets a widget that renders and reports its value but
    // never notifies — which is strictly better than the silent no-op that
    // was here before.

    fn attach_listener(
        view_ptr: *mut std::os::raw::c_void,
        class_name: &str,
        setter: &str,
        iface: &str,
        ctor_sig: &str,
        ctor_args: &[jni::objects::JValue<'_, '_>],
    ) {
        if view_ptr.is_null() {
            return;
        }
        let _ = with_env_and_activity(|env, _activity| {
            let cls = match load_app_class(env, class_name) {
                Ok(c) => c,
                Err(_) => {
                    let _ = env.exception_clear();
                    logcat_rs(&format!("listener class {class_name} not found"));
                    return Ok(());
                }
            };
            let listener = env.new_object(&cls, ctor_sig, ctor_args)?;
            // Keep a global ref: the platform holds the listener past this
            // call and a collected one is a silently dead callback rather
            // than a crash.
            PLATFORM_LISTENERS
                .lock()
                .unwrap()
                .push(env.new_global_ref(&listener)?);
            let view = unsafe { jni::objects::JObject::from_raw(view_ptr as jni::sys::jobject) };
            let setter_sig = format!("({iface})V");
            env.call_method(&view, setter, &setter_sig, &[(&listener).into()])?;
            Ok::<(), Box<dyn StdError + Send + Sync>>(())
        });
    }

    /// Install `OnItemSelectedListener` on a `Spinner`, dispatching to the
    /// UI-thread callback registered under `cb_id`.
    pub fn attach_item_selected_listener(view_ptr: *mut std::os::raw::c_void, cb_id: u64) {
        attach_listener(
            view_ptr,
            "com.corro.CorroItemSelected",
            "setOnItemSelectedListener",
            "android/widget/AdapterView$OnItemSelectedListener",
            "(J)V",
            &[(cb_id as i64).into()],
        );
    }

    /// Install `OnCheckedChangeListener` on a `CheckBox` / `RadioButton`.
    pub fn attach_checked_listener(view_ptr: *mut std::os::raw::c_void, cb_id: u64) {
        attach_listener(
            view_ptr,
            "com.corro.CorroChecked",
            "setOnCheckedChangeListener",
            "android/widget/CompoundButton$OnCheckedChangeListener",
            "(J)V",
            &[(cb_id as i64).into()],
        );
    }

    /// Install `OnScrollChangeListener` on a scrolled window.
    ///
    /// The id is the *window* pointer, not a registry id: the adapter's
    /// scroll registry is keyed by the view, and a `ScrollView` has at most
    /// one handler, so an id indirection would only add a way for the two
    /// keys to disagree.
    pub fn attach_scroll_listener(view_ptr: *mut std::os::raw::c_void) {
        attach_listener(
            view_ptr,
            "com.corro.CorroScrolled",
            "setOnScrollChangeListener",
            "android/view/View$OnScrollChangeListener",
            "(J)V",
            &[(view_ptr as i64).into()],
        );
    }

    /// Keep the platform listener objects alive; without a global ref the
    /// platform would collect one that is still installed on a live view.
    static PLATFORM_LISTENERS: Lazy<Mutex<Vec<GlobalRef>>> = Lazy::new(|| Mutex::new(Vec::new()));

    // ---- Dialog content / response plumbing ----

    /// The vertical container a dialog's children are added to, created on
    /// first use and remembered per builder.
    pub fn dialog_content_area(builder_ptr: *mut std::os::raw::c_void) -> jni::sys::jobject {
        if builder_ptr.is_null() {
            return std::ptr::null_mut();
        }
        if let Some(existing) = DIALOG_CONTENT.lock().unwrap().get(&(builder_ptr as usize)) {
            if existing.0 != std::ptr::null_mut() {
                return existing.0;
            }
        }
        let mut created = std::ptr::null_mut();
        let _ = with_env_and_activity(|env, activity| {
            let ctx = activity.as_obj();
            let layout = env.new_object(
                "android/widget/LinearLayout",
                "(Landroid/content/Context;)V",
                &[(&ctx).into()],
            )?;
            // A phone dialog stacks its content vertically: the rows are
            // labels, entries and toggles, and a horizontal box would put them
            // side by side off the right edge.
            let vertical = env.get_static_field("android/widget/LinearLayout", "VERTICAL", "I")?.i()?;
            env.call_method(&layout, "setOrientation", "(I)V", &[vertical.into()])?;
            let padding = (16.0 * display_density_or(env, activity, 1.0) as f32) as i32;
            env.call_method(
                &layout,
                "setPadding",
                "(IIII)V",
                &[padding.into(), padding.into(), padding.into(), 0i32.into()],
            )?;
            created = make_global_ref(env, &layout)?;
            Ok::<(), Box<dyn StdError + Send + Sync>>(())
        });
        if created != std::ptr::null_mut() {
            DIALOG_CONTENT
                .lock()
                .unwrap()
                .insert(builder_ptr as usize, RawJob(created));
        }
        created
    }

    /// The display density, or `fallback` when it cannot be read. Small
    /// helper so the dialog padding does not need the whole `DisplayMetrics`
    /// walk spelled out at each call site.
    fn display_density_or(env: &mut JNIEnv<'_>, activity: &GlobalRef, fallback: f64) -> f64 {
        env.call_method(
            activity.as_obj(),
            "getResources",
            "()Landroid/content/res/Resources;",
            &[],
        )
        .ok()
        .and_then(|r| r.l().ok())
        .and_then(|res| {
            env.call_method(
                &res,
                "getDisplayMetrics",
                "()Landroid/util/DisplayMetrics;",
                &[],
            )
            .ok()
            .and_then(|m| m.l().ok())
        })
        .and_then(|metrics| env.get_field(&metrics, "density", "F").ok())
        .and_then(|f| f.f().ok())
        .map(|d| d as f64)
        .unwrap_or(fallback)
    }

    /// A dialog's click listener for one button role, created on demand by
    /// `Dialog::add_button`.
    ///
    /// The listener *is* per-role, because that is the only place the role
    /// exists: Android hands an `OnClickListener` the `DialogInterface` and a
    /// `which` constant but nothing identifying the button that was
    /// registered. So the role travels in the object and arrives in Rust as
    /// the string, which the adapter maps to the response id.
    pub fn dialog_listener(builder_ptr: *mut std::os::raw::c_void, role: &str) -> Option<jni::sys::jobject> {
        if builder_ptr.is_null() {
            return None;
        }
        with_env_and_activity(|env, _activity| {
            let cls = load_app_class(env, "com.corro.CorroDialogListener")?;
            let j_role = env.new_string(role)?;
            let listener = env.new_object(
                &cls,
                "(JLjava/lang/String;)V",
                &[(builder_ptr as i64).into(), (&j_role).into()],
            )?;
            let gref = env.new_global_ref(&listener)?;
            let raw = gref.as_obj().as_raw();
            DIALOG_LISTENERS.lock().unwrap().push(gref);
            Ok(raw)
        })
        .ok()
    }

    /// Global refs for the dialog click listeners. One per (dialog, role);
    /// kept alive because the platform holds the listener past the
    /// `add_button` call and a collected one is a dead button.
    static DIALOG_LISTENERS: Lazy<Mutex<Vec<GlobalRef>>> = Lazy::new(|| Mutex::new(Vec::new()));
    /// Mirror of `crate::backends_android_adapter`'s table, so `add_button`
    /// can read the listener raw pointer without depending on the adapter
    /// module (which is `#[cfg(target_os = "android")]` inside a different
    /// module tree).
    /// A raw `jobject` in a `'static` map. The pointer is only ever a global
    /// ref kept alive by [`KEEP_ALIVE`] (see `make_global_ref`), and every
    /// use is on the UI thread; the `Send` impl is justified by the mutex
    /// plus that single-threaded access, exactly like the `Send*Callback`
    /// newtypes in the adapter.
    #[derive(Clone, Copy)]
    pub struct RawJob(pub jni::sys::jobject);
    // SAFETY: see the type's doc comment.
    unsafe impl Send for RawJob {}

    static DIALOG_CONTENT: Lazy<Mutex<HashMap<usize, RawJob>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    /// Tag a view with the Rust handle its listeners were built from, so
    /// `CorroKeyBridge` can recover it from Java.
    ///
    /// JNI cannot read a Rust field off a Java object, and the key bridge is
    /// handed a `View` (not a handle) because that is what `onKeyDown`
    /// receives. The pointer therefore has to live somewhere both sides can
    /// see it, and a view tag is the platform's own mechanism for exactly
    /// this. `0` for a view that is not a corro widget, which the Rust side
    /// reads as "no entry".
    pub fn tag_view_pointer(view_ptr: *mut std::os::raw::c_void) {
        if view_ptr.is_null() {
            return;
        }
        let _ = with_env_and_activity(|env, _activity| {
            let view = unsafe { jni::objects::JObject::from_raw(view_ptr as jni::sys::jobject) };
            // `View.setTag(Object)` is the generic form; the id-keyed
            // setTag(int, Object) would collide with a host's own use of a
            // resource id, so the plain slot is used and documented as ours.
            let tag = env.new_object("java/lang/Long", "(J)V", &[(view_ptr as i64).into()])?;
            env.call_method(&view, "setTag", "(Ljava/lang/Object;)V", &[(&tag).into()])?;
            Ok::<(), Box<dyn StdError + Send + Sync>>(())
        });
    }

    /// Install the `OnKeyListener` on an entry so raw keys reach corro's edit
    /// model. The Java shim is `com.corro.CorroKeyListener`, already wired by
    /// `attach_editor_action`; this only has to make sure the class is present
    /// for an entry that got a key handler *before* an activate handler.
    pub fn attach_key_listener(entry_ptr: *mut std::os::raw::c_void) {
        if entry_ptr.is_null() {
            return;
        }
        let _ = with_env_and_activity(|env, _activity| {
            let entry = unsafe { jni::objects::JObject::from_raw(entry_ptr as jni::sys::jobject) };
            let cls = match load_app_class(env, "com/corro/CorroKeyListener") {
                Ok(c) => c,
                Err(_) => {
                    let _ = env.exception_clear();
                    return Ok(());
                }
            };
            let listener = env.new_object(&cls, "(J)V", &[(entry_ptr as i64).into()])?;
            let gref = env.new_global_ref(&listener)?;
            ENTRY_KEY_LISTENERS.lock().unwrap().push(gref);
            env.call_method(
                &entry,
                "setOnKeyListener",
                "(Landroid/view/View$OnKeyListener;)V",
                &[(&listener).into()],
            )?;
            Ok::<(), Box<dyn StdError + Send + Sync>>(())
        });
    }

    static ENTRY_KEY_LISTENERS: Lazy<Mutex<Vec<GlobalRef>>> =
        Lazy::new(|| Mutex::new(Vec::new()));

    // ---- System clipboard ----

    /// Put text on the Android system clipboard (`ClipboardManager`).
    ///
    /// Returns whether it worked, so a caller can report the copy as
    /// in-app-only rather than silently claiming it went to the system. That
    /// distinction matters: a copy that appears to succeed and cannot be
    /// pasted anywhere is worse than one that says so.
    pub fn clipboard_set_text(text: &str) -> bool {
        with_env_and_activity(|env, _activity| {
            let cls = load_app_class(env, "com.corro.CorroClipboard")?;
            let j_text = env.new_string(text)?;
            let ok = env
                .call_static_method(&cls, "setText", "(Ljava/lang/String;)Z", &[(&j_text).into()])?
                .z()?;
            Ok::<bool, Box<dyn StdError + Send + Sync>>(ok)
        })
        .unwrap_or(false)
    }

    /// The system clipboard's plain text, or `None` when it holds nothing.
    ///
    /// `None` rather than `Some("")` for an empty clipboard, because those
    /// are different operations for a caller: pasting nothing is a no-op,
    /// while pasting an empty string into a cell is a real (destructive) edit.
    pub fn clipboard_get_text() -> Option<String> {
        with_env_and_activity(|env, _activity| {
            let cls = load_app_class(env, "com.corro.CorroClipboard")?;
            let value = env
                .call_static_method(&cls, "getText", "()Ljava/lang/String;", &[])?
                .l()?;
            if value.is_null() {
                return Ok(None);
            }
            let s = unsafe { jni::objects::JObject::from_raw(value.as_raw()) };
            let out: String = env.get_string(&JString::from(s))?.into();
            Ok::<Option<String>, Box<dyn StdError + Send + Sync>>(Some(out))
        })
        .ok()
        .flatten()
    }

    // ---- Storage Access Framework ----

    /// Bind the SAF picker to the Activity. The host calls this from
    /// `onCreate`; rswidgets cannot subclass the host's Activity, so the
    /// picker is reached through a host-supplied class rather than
    /// rswidgets' own.
    pub fn attach_file_picker() {
        let _ = with_env_and_activity(|env, activity| {
            let cls = match load_app_class(env, "com.corro.CorroFile") {
                Ok(c) => c,
                Err(_) => {
                    let _ = env.exception_clear();
                    return Ok(());
                }
            };
            env.call_static_method(&cls, "attach", "(Landroid/app/Activity;)V",
                &[activity.as_obj().into()])?;
            Ok::<(), Box<dyn StdError + Send + Sync>>(())
        });
    }

    /// Build the `String[]` a `CorroFile.open` call takes from a list of
    /// `&str`. `None` for an empty list, so the Java side can tell "no filter
    /// requested" (any file) from "filter of one empty string".
    fn jni_string_array<'a>(
        env: &mut JNIEnv<'a>,
        values: &[String],
    ) -> Result<jni::objects::JObjectArray<'a>, Box<dyn StdError + Send + Sync>> {
        let arr = env.new_object_array(
            values.len() as i32,
            "java/lang/String",
            jni::objects::JObject::null(),
        )?;
        for (i, v) in values.iter().enumerate() {
            let j = env.new_string(v)?;
            env.set_object_array_element(&arr, i as i32, &j)?;
        }
        Ok(arr)
    }

    /// Show the platform's open-document picker and return the chosen
    /// `content://` URI, or `None` if the user cancelled.
    ///
    /// This is the Android spelling of `open_file`. Note the difference that
    /// makes it more than a rename: the result is a **URI, not a path** —
    /// there is no filesystem path behind it, and a `content://` string
    /// cannot be opened with `std::fs`. Callers use
    /// [`materialize_document`] to get a real file, or
    /// [`write_document`] to write one back.
    pub fn open_document(mime_types: &[String]) -> Option<String> {
        with_env_and_activity(|env, _activity| {
            let cls = load_app_class(env, "com.corro.CorroFile")?;
            let arr = jni_string_array(env, mime_types)?;
            let uri = env
                .call_static_method(&cls, "open", "([Ljava/lang/String;)Ljava/lang/String;", &[(&arr).into()])?
                .l()?;
            if uri.is_null() {
                return Ok(None);
            }
            let uri = unsafe { jni::objects::JObject::from_raw(uri.as_raw()) };
            let s: String = env.get_string(&JString::from(uri))?.into();
            Ok(Some(s))
        })
        .ok()
        .flatten()
    }

    /// Show the platform's create-document picker and return the chosen
    /// `content://` URI. This is `save_file`.
    pub fn create_document(mime: &str, default_name: &str) -> Option<String> {
        with_env_and_activity(|env, _activity| {
            let cls = load_app_class(env, "com.corro.CorroFile")?;
            let j_mime = env.new_string(mime)?;
            let j_name = env.new_string(default_name)?;
            let uri = env
                .call_static_method(
                    &cls,
                    "create",
                    "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
                    &[(&j_mime).into(), (&j_name).into()],
                )?
                .l()?;
            if uri.is_null() {
                return Ok(None);
            }
            let uri = unsafe { jni::objects::JObject::from_raw(uri.as_raw()) };
            let s: String = env.get_string(&JString::from(uri))?.into();
            Ok(Some(s))
        })
        .ok()
        .flatten()
    }

    /// Read a document's bytes through the content resolver. Empty on any
    /// failure — an empty workbook is indistinguishable from an empty file
    /// here, which is the same failure the caller would get from an empty
    /// path on a desktop.
    pub fn read_document(uri: &str) -> Vec<u8> {
        with_env_and_activity(|env, _activity| {
            let cls = load_app_class(env, "com.corro.CorroFile")?;
            let j_uri = env.new_string(uri)?;
            let bytes = env
                .call_static_method(&cls, "readBytes", "(Ljava/lang/String;)[B", &[(&j_uri).into()])?
                .l()?;
            if bytes.is_null() {
                return Ok(Vec::new());
            }
            let arr = unsafe {
                jni::objects::JByteArray::from(
                    jni::objects::JObject::from_raw(bytes.as_raw()),
                )
            };
            let len = env.get_array_length(&arr)? as i32;
            let mut buf = vec![0i8; len.max(0) as usize];
            if len > 0 {
                env.get_byte_array_region(&arr, 0, &mut buf)?;
            }
            Ok(buf.into_iter().map(|b| b as u8).collect())
        })
        .unwrap_or_default()
    }

    /// Overwrite a document's contents. `false` on failure; the caller
    /// surfaces it as a save error rather than a silent success.
    pub fn write_document(uri: &str, data: &[u8]) -> bool {
        with_env_and_activity(|env, _activity| {
            let cls = load_app_class(env, "com.corro.CorroFile")?;
            let j_uri = env.new_string(uri)?;
            let bytes = env.byte_array_from_slice(data)?;
            let ok = env
                .call_static_method(
                    &cls,
                    "writeBytes",
                    "(Ljava/lang/String;[B)Z",
                    &[(&j_uri).into(), (&bytes).into()],
                )?
                .z()?;
            Ok::<bool, Box<dyn StdError + Send + Sync>>(ok)
        })
        .unwrap_or(false)
    }

    /// Copy a document into the app's private storage and return the real
    /// path.
    ///
    /// This is what makes the *import* direction work without touching the
    /// loaders: they all take a `Path` and use `std::fs`, and a `content://`
    /// URI is not one. Copying once means a workbook opened from Drive or
    /// Downloads can be read by the existing code unchanged, and the
    /// persistable permission is not needed for the read.
    pub fn materialize_document(uri: &str, file_name: &str) -> Option<String> {
        with_env_and_activity(|env, _activity| {
            let cls = load_app_class(env, "com.corro.CorroFile")?;
            let j_uri = env.new_string(uri)?;
            let j_name = env.new_string(file_name)?;
            let path = env
                .call_static_method(
                    &cls,
                    "materialize",
                    "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
                    &[(&j_uri).into(), (&j_name).into()],
                )?
                .l()?;
            if path.is_null() {
                return Ok(None);
            }
            let path = unsafe { jni::objects::JObject::from_raw(path.as_raw()) };
            let out: String = env.get_string(&JString::from(path))?.into();
            Ok(Some(out))
        })
        .ok()
        .flatten()
    }

    /// Forward the host Activity's `onActivityResult` to the SAF picker.
    ///
    /// The platform delivers the picker's answer to the Activity, and
    /// rswidgets cannot subclass the host's, so the host's `onActivityResult`
    /// calls this. Without it `startActivityForResult`'s result is dropped
    /// and every dialog returns "cancelled".
    pub fn forward_file_picker_result(
        request: i32,
        result_code: i32,
        data: &JObject<'_>,
    ) {
        let _ = with_env_and_activity(|env, _activity| {
            let cls = match load_app_class(env, "com.corro.CorroFile") {
                Ok(c) => c,
                Err(_) => {
                    let _ = env.exception_clear();
                    return Ok(());
                }
            };
            env.call_static_method(
                &cls,
                "onActivityResult",
                "(IILandroid/content/Intent;)V",
                &[request.into(), result_code.into(), data.into()],
            )?;
            Ok::<(), Box<dyn StdError + Send + Sync>>(())
        });
    }

    pub fn attach_child(container_ptr: *mut std::os::raw::c_void, child_ptr: *mut std::os::raw::c_void) {
        if container_ptr.is_null() || child_ptr.is_null() {
            return;
        }
        let _ = with_env_and_activity(|env, _activity| {
            let container =
                unsafe { jni::objects::JObject::from_raw(container_ptr as jni::sys::jobject) };
            let child =
                unsafe { jni::objects::JObject::from_raw(child_ptr as jni::sys::jobject) };
            env.call_method(
                &container,
                "addView",
                "(Landroid/view/View;)V",
                &[(&child).into()],
            )?;
            Ok::<_, Box<dyn StdError + Send + Sync>>(())
        });
    }

    /// Inert `FrameLayout` container backing a scrolled window. Corro drives
    /// its own viewport, so this never scrolls natively — it only keeps the
    /// shared GUI layout code compiling unchanged with valid handles.
    pub fn create_scrolled_view() -> Result<jni::sys::jobject, Box<dyn StdError + Send + Sync>> {
        with_env_and_activity(|env, activity| {
            let ctx = activity.as_obj();
            let layout = env.new_object(
                "android/widget/FrameLayout",
                "(Landroid/content/Context;)V",
                &[(&ctx).into()],
            )?;
            make_global_ref(env, &layout)
        })
    }
}

#[cfg(target_os = "android")]
pub use android_backend::*;

#[cfg(test)]
#[cfg(target_os = "android")]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_invoke_callback() {
        let called = std::cell::Cell::new(false);
        let id = register_callback(Box::new(move || {
            called.set(true);
        }));

        // Manually invoke (simulates Java dispatchCallback)
        invoke_callback(id);

        // Note: in a real test with a real closure we'd need different approach
        // since the closure is consumed by the Box. This test checks the dispatch
        // mechanism doesn't panic.
    }

    #[test]
    fn test_register_and_unregister() {
        let id = register_callback(Box::new(|| {}));
        unregister_callback(id);
        // After unregister, invoke should be a no-op (not panic)
        invoke_callback(id);
    }

    #[test]
    fn test_dispatch_callback() {
        let id = register_callback(Box::new(|| {}));
        dispatch_callback(id);
        unregister_callback(id);
    }

    #[test]
    fn test_multiple_callbacks() {
        let id1 = register_callback(Box::new(|| {}));
        let id2 = register_callback(Box::new(|| {}));
        assert_ne!(id1, id2);
        invoke_callback(id1);
        invoke_callback(id2);
        unregister_callback(id1);
        unregister_callback(id2);
    }

    #[test]
    fn test_is_initialized() {
        // Before init, should be false
        assert!(!is_initialized());
    }
}
