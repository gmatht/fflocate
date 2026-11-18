#[cfg(target_arch = "wasm32")]
mod wasm_adapter {
    use std::any::Any;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::os::raw::c_void;
    use std::rc::Rc;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;
    use web_sys::{
        Document, Element, Event, FocusEvent, HtmlAnchorElement, HtmlCanvasElement,
        HtmlButtonElement, HtmlDialogElement, HtmlDivElement, HtmlElement,
        HtmlInputElement, HtmlOptionElement, HtmlSelectElement, HtmlTextAreaElement,
        KeyboardEvent, MouseEvent,
    };
    use crate::core::{Error, Widget};

    // -----------------------------------------------------------------------
    // Global helpers
    // -----------------------------------------------------------------------
    fn document() -> Document {
        web_sys::window().unwrap().document().unwrap()
    }

    fn body() -> HtmlElement {
        document().body().unwrap()
    }

    fn create_element(tag: &str) -> Element {
        document().create_element(tag).unwrap()
    }

    // -----------------------------------------------------------------------
    // File dialogs
    //
    // A browser has no filesystem paths, so the path-returning
    // `App::open_file` / `save_file` cannot be implemented here and stay
    // honest no-ops. What a page *can* do is hand the app the file's
    // *contents*, and that is what these two do: `open_file_content` runs a
    // real file picker and reads the choice as text, `save_file_content`
    // builds a Blob and clicks a synthetic <a download> so the browser writes
    // it wherever the user's download settings say. A caller that needs a
    // path keeps needing one it cannot get; a caller that needs a document
    // gets one.
    // -----------------------------------------------------------------------

    /// Ask the user to pick a file and read it, calling
    /// `on_chosen(name, Some(text))` -- or `on_chosen(name, None)` if the user
    /// cancels.
    ///
    /// The three constraints that shape this, all inherent to a browser:
    ///
    /// 1. *The picker is modal and async.* There is no `Result` to return
    ///    from, so the answer arrives as a callback. The sync
    ///    `open_file` signature cannot express this, which is why it stays a
    ///    no-op rather than pretending.
    /// 2. *A chooser must be triggered by a user gesture.* Browsers suppress
    ///    the dialog otherwise, so the input is `.click()`ed synchronously
    ///    from inside this call -- the caller has to be in an event handler.
    /// 3. *The name is not a path.* `File::name` is a bare basename, so
    ///    `on_chosen` is given the name and the text separately, and the
    ///    caller decides what to do with the name (title the document, suggest
    ///    it on the next save).
    ///
    /// `accept` is the `accept` attribute: a comma-separated list of
    /// extensions and/or MIME types (".corro,.ods,text/csv"). It is a *hint*
    /// that greys the rest of the list; the browser still allows All files,
    /// which is the same contract the GTK filter has.
    pub fn open_file_content(
        accept: Option<&str>,
        on_chosen: Box<dyn FnMut(Option<String>, Option<String>)>,
    ) -> Result<(), Error> {
        let input: HtmlInputElement = create_element("input").dyn_into().map_err(|e| {
            Error::Backend(format!("open_file_content: {e:?}"))
        })?;
        input.set_type("file");
        if let Some(acc) = accept {
            input.set_accept(acc);
        }
        // Kept in the DOM (not just referenced) for the picker's lifetime:
        // detaching it can cancel the dialog in some browsers.
        set_css(&input, "display", "none");
        body().append_child(&input).ok();

        let cell = Rc::new(RefCell::new(Some(on_chosen)));
        let on_change = cell.clone();
        let input_for_handler = input.clone();
        let closure = Closure::<dyn FnMut(Event)>::new(move |_evt: Event| {
            // Run exactly once: the input fires `change` on pick *and* on
            // cancel in some browsers, and a second call would re-enter the
            // caller's handler with a stale value.
            let handler = on_change.borrow_mut().take();
            let Some(mut handler) = handler else {
                return;
            };
            let files = input_for_handler.files();
            let Some(file) = files.and_then(|f| f.item(0)) else {
                // No file: the user dismissed the dialog.
                handler(None, None);
                return;
            };
            let name = file.name();
            let reader = match web_sys::FileReader::new() {
                Ok(r) => r,
                Err(_) => {
                    handler(Some(name), None);
                    return;
                }
            };
            let name_for_result = name.clone();
            let on_done = cell.clone();
            // The reader is moved into the onload closure so it cannot be
            // dropped before the load completes -- `FileReader::result` after a
            // drop would be a use-after-free across the FFI boundary, and
            // browsers do not keep a JS object alive for a pending callback.
            // `FileReader` is a JsCast handle: cloning it is a refcount bump
            // on the same JS object, so the closure and the setup code below
            // can each hold one safely.
            let reader_for_cb = reader.clone();
            let read_ok = Closure::<dyn FnMut(Event)>::new(move |_evt: Event| {
                let text = reader_for_cb
                    .result()
                    .ok()
                    .and_then(|v| v.as_string());
                if let Some(mut h) = on_done.borrow_mut().take() {
                    h(Some(name_for_result.clone()), text);
                }
            });
            // `read_as_text` is what starts the load, so it has to come after
            // `onload` is attached -- and both have to happen before `reader`
            // is moved into the closure above. Ordering matters twice here:
            // an `onload` set after `read_as_text` can miss a cached read.
            let _ = reader.set_onload(Some(read_ok.as_ref().unchecked_ref()));
            let _ = reader.read_as_text(&file);
            // Leaked deliberately: `onload` fires long after this function has
            // returned, and dropping the Closure would free the trampoline it
            // points at. The load path runs once per pick, so this is bounded
            // by the number of files the user opens.
            std::mem::forget(read_ok);
        });
        input
            .add_event_listener_with_callback("change", closure.as_ref().unchecked_ref())
            .ok();
        std::mem::forget(closure);

        // The gesture-triggered click, per constraint 2 above.
        input.click();
        Ok(())
    }

    /// Offer `text` as a download named `filename`, via a Blob and a
    /// synthetic <a download> click.
    ///
    /// The DOM equivalent of writing a file: the browser decides where it
    /// lands (its own download directory, or a save dialog if the page has
    /// asked for one), and the app never learns the path -- which is honest,
    /// because there is no path to learn. Returns once the click is
    /// dispatched; the download itself is the browser's to complete.
    pub fn save_file_content(filename: &str, text: &str) -> Result<(), Error> {
        let parts = js_sys::Array::new();
        parts.push(&wasm_bindgen::JsValue::from_str(text));
        let opts = web_sys::BlobPropertyBag::new();
        opts.set_type("text/plain;charset=utf-8");
        let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &opts)
            .map_err(|e| Error::Backend(format!("save_file_content: {e:?}")))?;
        let url = web_sys::Url::create_object_url_with_blob(&blob)
            .map_err(|e| Error::Backend(format!("create_object_url: {e:?}")))?;

        let anchor: HtmlAnchorElement = create_element("a").dyn_into().map_err(|e| {
            Error::Backend(format!("save_file_content anchor: {e:?}"))
        })?;
        anchor.set_href(&url);
        anchor.set_download(filename);
        set_css(&anchor, "display", "none");
        body().append_child(&anchor).ok();
        anchor.click();
        anchor.remove();

        // Revoking immediately would race the download in some browsers; the
        // URL is released when the document is, which is the documented
        // escape for a one-shot object URL.
        Ok(())
    }

    /// Run `f` every `ms` milliseconds until it returns `false`.
    ///
    /// `setInterval`, and the closure is leaked: an interval has no natural
    /// owner here (the backend is a set of free functions, not a window), and
    /// the `true` return from `f` is what stops it. Leaking the `Closure` is
    /// therefore not a leak in practice -- a repeating tick is meant to
    /// outlive the call that armed it, and the interval handle stops firing
    /// as soon as `f` says so.
    ///
    /// `ms` is clamped to 1: `setInterval` treats 0 as "as fast as the
    /// browser allows" and a negative value as 0, so a caller asking for
    /// 0 ms would get an unthrottled loop rather than the one-shot-per-tick
    /// behaviour a 0 interval suggests.
    pub fn add_periodic_tick(
        ms: u32,
        f: Box<dyn FnMut() -> bool>,
    ) -> Result<(), Error> {
        let cell = Rc::new(RefCell::new(f));
        // The interval id, shared with the callback so that returning `false`
        // can actually cancel the interval.
        //
        // This has to exist: `setInterval` keeps firing until something calls
        // `clearInterval`, and the only thing that knows the tick is finished
        // is `f` itself. A cell holding the id is set *after* the first
        // `setInterval` call, and a tick that fires before that reads 0 --
        // which is not a valid interval id, so `clearInterval(0)` is a no-op
        // and the next scheduled run does the cancelling. In practice the
        // delay is `ms`, far longer than the microseconds the assignment
        // needs, so the common case cancels on the first tick.
        let handle: Rc<Cell<i32>> = Rc::new(Cell::new(0));
        let handle_for_cb = handle.clone();
        let closure = Closure::<dyn FnMut()>::new(move || {
            // A panic inside `f` would unwind through a JS callback, which
            // wasm-bindgen cannot do safely. `try_borrow_mut` degrades to a
            // skipped tick instead, which is also what a re-entrant call from
            // `f` needs.
            let keep_going = match cell.try_borrow_mut() {
                Ok(mut guard) => guard(),
                Err(_) => true,
            };
            if !keep_going {
                if let Some(w) = web_sys::window() {
                    w.clear_interval_with_handle(handle_for_cb.get());
                }
            }
        });
        let ms = ms.max(1);
        let Some(w) = web_sys::window() else {
            return Err(Error::Backend("no window: no event loop to schedule on".into()));
        };
        let id = w
            .set_interval_with_callback_and_timeout_and_arguments_0(
                closure.as_ref().unchecked_ref(),
                ms as i32,
            )
            .map_err(|e| Error::Backend(format!("setInterval: {e:?}")))?;
        handle.set(id);
        // Keep the closure alive for as long as the page is; the interval
        // owns the callback, and a dropped `Closure` would free the JS
        // trampoline that callback still points at.
        std::mem::forget(closure);
        Ok(())
    }

    /// Run `f` once, `ms` milliseconds from now.
    pub fn timeout_add_once(ms: u32, f: Box<dyn FnOnce()>) -> Result<(), Error> {
        let closure = Closure::<dyn FnMut()>::once_into_js(move || f());
        let ms = ms.max(1) as i32;
        let _ = web_sys::window().map(|w| {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                closure.as_ref().unchecked_ref(),
                ms,
            );
        });
        std::mem::forget(closure);
        Ok(())
    }

    /// A repeating tick on `window`. `timeout_add_repeating` is the GTK and
    /// NWG spelling; `add_periodic_tick` above is what `core.rs` calls.
    pub fn timeout_add_repeating(ms: u32, f: Box<dyn FnMut() -> bool>) -> Result<(), Error> {
        add_periodic_tick(ms, f)
    }

    pub fn quit_main_loop() {
        // On WASM there is no message loop to quit.  Signal the test
        // framework that the app has quit, and try to close the tab.
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            // Set document title as a signal for test frameworks
            // that may poll for it.
            doc.set_title("CORRO_QUIT");
            // `window.close()` below is refused for a tab the script did not
            // open, which is every tab a user navigated to. Take the app's own
            // root out of the page so "Exit" visibly exits; see
            // `Window::hide`.
            if let Some(root) = doc
                .body()
                .and_then(|b| b.first_element_child())
                .filter(|e| e.tag_name() != "SCRIPT")
            {
                let _ = root.remove();
            }
        }
        // Deliberately NOT `window.close()`: a browser refuses it for a tab
        // the script did not open (every tab a user navigated to), and Chrome
        // logs the refusal to the console *after* the caller of this function
        // returns -- which the shared GUI turns into a panic, because it exits
        // the process straight after quitting. So the tab stays open (a page
        // cannot close itself) and the title above is what tells a caller the
        // app has quit.
    }

    /// Honour a repaint requested from inside the frame that just finished.
    ///
    /// The host calls this once per frame, after its draw callback returns. A
    /// redraw requested *during* a paint cannot run in that paint (it would
    /// recurse) and must not be dropped, or a change would stay on screen only
    /// until some later input. One more frame is scheduled here; in practice
    /// that frame requests nothing and the chain ends.
    pub fn flush_pending_frame() {
        if !REDRAW_AFTER_FRAME.with(|f| f.replace(false)) {
            return;
        }
        let canvas = LAST_CANVAS.with(|c| c.borrow_mut().take());
        if let Some(canvas) = canvas {
            canvas.redraw_pending.set(false);
            canvas.queue_redraw();
        }
    }

    /// Store a line of output in a JS global (`window.__corro_output`).
    /// The browser test framework can read this variable after the app quits
    /// to verify the recording replay output.
    pub fn append_wasm_output_line(line: &str) {
        if let Some(win) = web_sys::window() {
            let _ = js_sys::Reflect::set(
                &win,
                &wasm_bindgen::JsValue::from_str("__corro_output_dirty"),
                &wasm_bindgen::JsValue::TRUE,
            );
            let current = js_sys::Reflect::get(
                &win,
                &wasm_bindgen::JsValue::from_str("__corro_output"),
            )
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default();
            let new = if current.is_empty() {
                line.to_string()
            } else {
                format!("{}\n{}", current, line)
            };
            let _ = js_sys::Reflect::set(
                &win,
                &wasm_bindgen::JsValue::from_str("__corro_output"),
                &wasm_bindgen::JsValue::from_str(&new),
            );
        }
    }

    /// The pointer's position inside the canvas, in **style pixels**.
    ///
    /// `offset_x`/`offset_y` are already the event's position in the canvas'
    /// own CSS box -- the space every drawing call works in (a row is 20px, a
    /// column 7.2px), because the 2d context installs the only
    /// `devicePixelRatio` transform there is.
    ///
    /// Scaling them by the element's backing store (`width` / CSS width) was
    /// the "a click lands about twice as far from the top-left as the cursor"
    /// bug on a 2x display: the hit test compared the doubled numbers against
    /// a grid drawn at 1x, so every press resolved `dpr` rows and columns
    /// down and right of the pointer. The scale was also read before the first
    /// frame assigned the backing store, so the first press after a load was
    /// tested against a 300x150 default.
    fn local_point(off_x: f64, off_y: f64, _css_size: &Cell<(f64, f64)>, _elem: &HtmlCanvasElement) -> (f64, f64) {
        (off_x, off_y)
    }

    /// Let native `scroll` events reach `target` from `outer`'s scrolling.
    ///
    /// `ScrolledWindow` here is a real overflow container, so the element that
    /// receives a user's scroll and fires `scroll` is the *container*, not the
    /// canvas inside it. A listener registered on the canvas therefore never
    /// sees a wheel or a touchpad drag. These three properties turn the wheel
    /// and the scrollbar into a native scroll of the container, and then hand
    /// the event to `target`: the container keeps its scrollbar (and hides it,
    /// because the app paints the thumb itself), while every delta arrives on
    /// the canvas, which is where `on_gesture` listens.
    fn forward_scroll_from(outer: &HtmlElement, target: &Element) {
        // The *canvas* decides where a scroll goes, so the container must not
        // scroll itself: a browser that scrolls it would move the drawing
        // surface out from under the pixels the app has already painted, and
        // `clientWidth`/`clientHeight` -- the size the backing store follows --
        // would shrink with it.
        set_css(outer, "scrollbar-width", "none");
        set_css(outer, "overscroll-behavior", "contain");
        set_css(outer, "touch-action", "none");
        let _ = target;
    }

    /// Set a menu label's text, interpreting a GTK mnemonic marker.
    ///
    /// The shared menu tree produces GTK labels (`_File`, `S_ave as`) whose
    /// underscore marks the letter `Alt` activates. A page has no underline,
    /// so the marker has to be rendered *as* one: `_File` becomes a
    /// first-letter `<u>` element and `S_ave as` an underlined `a`. Rendering
    /// the raw string (what this did) put a control character in every menu
    /// item -- `_File`, `S_ave as` -- which is the bug this fixes.
    ///
    /// `innerHTML` rather than `textContent` because the underline is an
    /// element; every label character is HTML-escaped first, so the only tag
    /// in the result is the `<u>` introduced here.
    ///
    /// Mnemonics are still derived from the *original* label (with its marker)
    /// in `activate_submenu_by_mnemonic`, so the visible label and the working
    /// key cannot drift apart.
    fn set_menu_label(elem: &Element, label: &str) {
        match label.char_indices().find(|(_, c)| *c == '_') {
            Some((i, _)) => {
                let (before, rest) = label.split_at(i);
                let mut after = rest.chars();
                after.next(); // the marker
                match after.next() {
                    Some(marked) => {
                        let tail: String = after.collect();
                        elem.set_inner_html(&format!(
                            "{}<u>{}</u>{}",
                            escape_html_text(before),
                            escape_html_text(&marked.to_string()),
                            escape_html_text(&tail),
                        ));
                    }
                    // A trailing marker marks nothing: show the text as-is.
                    None => elem.set_text_content(Some(&format!("{before}_"))),
                }
            }
            None => elem.set_text_content(Some(label)),
        }
    }

    /// Escape the three characters that could turn label text into markup.
    ///
    /// `set_menu_label` writes `innerHTML` in order to emit one `<u>` tag, so
    /// any `<`, `>` or `&` in a label (a future "Sort > Ascending", an item
    /// named "R&D") would otherwise be parsed as markup or swallowed.
    fn escape_html_text(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '&' => out.push_str("&amp;"),
                _ => out.push(c),
            }
        }
        out
    }

    /// Whether any submenu is open.
    fn menu_active_in(open: &OpenMenu) -> bool {
        open.borrow().is_some()
    }

    /// The value shared by every closure that has to open or close a submenu:
    /// the one "which submenu is open" cell for the whole bar.
    ///
    /// The cell is `(toggle, dropdown)` for the open entry, matching
    /// `MenuBar::open`. Toggle and item click handlers run outside any
    /// `MenuBar` method, so without a shared handle they could only flip the
    /// DOM `display` -- and the bar went on believing a menu was open after it
    /// had visibly closed. That was the "menus open but never close" symptom:
    /// clicking an item dispatched its action but left the menubar's state (and
    /// the next menu's geometry) untouched, and `Alt+letter` then reopened the
    /// same dropdown while Escape closed it only in the DOM.
    type OpenMenu = Rc<RefCell<Option<(Element, Element)>>>;

    /// Whether `dropdown` is the submenu the bar has open.
    fn menu_open_is(open: &OpenMenu, dropdown: &Element) -> bool {
        let borrowed = open.borrow();
        let Some((_, dd)) = borrowed.as_ref() else {
            return false;
        };
        // `is_same_node`: two `Element` handles for one DOM node are distinct
        // `JsValue`s, so `==` would compare identity rather than the node.
        let a: &web_sys::Node = dd;
        let b: &web_sys::Node = dropdown;
        a.is_same_node(Some(b))
    }

    /// Show `dropdown` and mark it open. The caller closes what was open
    /// first (`close_open_menu`), so this never leaves two menus visible.
    fn open_menu_entry(open: &OpenMenu, dropdown: &Element, toggle: Option<&Element>) {
        if let Some(html) = dropdown.dyn_ref::<HtmlElement>() {
            let _ = html.style().set_property("display", "block");
        }
        // The toggle is the dropdown's previous sibling: the two are always
        // created as a pair in `create_menubar`.
        let toggle = match toggle {
            Some(t) => t.clone(),
            None => dropdown
                .previous_element_sibling()
                .unwrap_or_else(|| dropdown.clone()),
        };
        if let Some(html) = toggle.dyn_ref::<HtmlElement>() {
            let _ = html.style().set_property("background", "#e8e8e8");
        }
        *open.borrow_mut() = Some((toggle, dropdown.clone()));
    }

    // The menubar's key guard: one live `keydown` listener, installed while a
    // menu is open.
    //
    // The formula entry keeps focus while a menu is open (a press on the
    // menubar never moves it), so the browser delivers in-menu keystrokes to a
    // text input -- and the input inserts the character however our own
    // handler is written, because suppressing a *text input's* default action
    // has to happen on `keydown`, upstream of both handlers.
    //
    // Measured, not assumed (headless Chromium, a page with one focused
    // `<input>` and nothing else): a `keydown` that is not prevented inserts
    // the character even when the input has no input/change/composition
    // listeners at all -- the insertion is the browser's default action, and
    // preventing `keydown` is the only thing that stops it.
    //
    // So while a menu is open the bar installs one `keydown` listener on
    // `window` that prevents the default for every key and routes it to
    // `MenuBar::handle_menu_key_char`; a key the menu does not consume goes on
    // to the app's own sink (`set_host_key_sink`), or Ctrl+Q and F1 would stop
    // working for as long as a menu is open.
    //
    // Kept in thread-locals so `close_open_menu` -- which every close path
    // goes through -- can remove the listener; one per open would otherwise
    // accumulate.
    thread_local! {
        /// True while a frame is being painted (see `Canvas::draw_now`).
        static FRAME_OPEN: Cell<bool> = const { Cell::new(false) };
        /// A repaint was requested from inside a frame, for the next one.
        static REDRAW_AFTER_FRAME: Cell<bool> = const { Cell::new(false) };
        /// The canvas that painted most recently, so a request made during its
        /// paint can be honoured once that paint returns.
        static LAST_CANVAS: RefCell<Option<Canvas>> = RefCell::new(None);
        /// A live listener, if a menu is open.
        static MENU_KEY_GUARD: RefCell<Option<js_sys::Function>> = RefCell::new(None);
        /// The bar whose menu is open, so the listener can route to it.
        static GUARDED_MENUBAR: RefCell<Option<MenuBar>> = RefCell::new(None);
        /// The app's own key sink, for the keys a menu does not consume.
        static HOST_KEY_SINK: RefCell<Option<Rc<dyn Fn(u32, char, u32) -> bool>>> =
            RefCell::new(None);
    }

    /// Installs the app's key sink, called by the menu key guard for every key
    /// an open menu does not take.
    ///
    /// Set once by the host after the window exists. Until it is, a key typed
    /// at an open menu that the menu does not want is dropped -- which is what
    /// the browser did with it anyway.
    pub fn set_host_key_sink(sink: Rc<dyn Fn(u32, char, u32) -> bool>) {
        HOST_KEY_SINK.with(|s| *s.borrow_mut() = Some(sink));
    }

    fn install_menu_key_guard(open: &OpenMenu, menubar: &MenuBar) {
        let Some(win) = web_sys::window() else {
            return;
        };
        remove_menu_key_guard();
        GUARDED_MENUBAR.with(|m| *m.borrow_mut() = Some(menubar.clone()));
        let closure = Closure::<dyn FnMut(KeyboardEvent)>::new(move |evt: KeyboardEvent| {
            // Modifier keys (Alt on its own, Shift) carry no character: letting
            // them through keeps the app's modifier tracking working, and they
            // insert nothing.
            if matches!(evt.key().as_str(), "Alt" | "Shift" | "Control" | "Meta") {
                return;
            }
            // The insertion has to be stopped on `keydown`, before the input
            // sees it -- preventing it on `keypress` would be too late, the
            // text is already queued.
            evt.prevent_default();
            let keyval = evt.key_code() as u32;
            let ch = typed_char(&evt);
            let mods = gdk_modifier_mask(&evt);
            let consumed = GUARDED_MENUBAR
                .with(|m| m.borrow().clone())
                .map(|bar| bar.handle_menu_key_char(keyval, ch, mods))
                .unwrap_or(false);
            if !consumed {
                // Not a menu key (Ctrl+Q, F1, an arrow the menu does not move
                // on): hand it to the app, which would otherwise never see it
                // -- the default is already prevented.
                let sink = HOST_KEY_SINK.with(|s| s.borrow().clone());
                if let Some(sink) = sink {
                    sink(keyval, ch, mods);
                }
            }
        });
        let func: js_sys::Function = closure.as_ref().unchecked_ref::<js_sys::Function>().clone();
        // The listener owns the JS trampoline for as long as it is attached;
        // `remove_menu_key_guard` detaches and drops both together.
        closure.forget();
        let _ = win.add_event_listener_with_callback("keydown", &func);
        MENU_KEY_GUARD.with(|g| *g.borrow_mut() = Some(func));
        let _ = open;
    }

    fn remove_menu_key_guard() {
        if let Some(func) = MENU_KEY_GUARD.with(|g| g.borrow_mut().take()) {
            if let Some(win) = web_sys::window() {
                let _ = win.remove_event_listener_with_callback("keydown", &func);
            }
        }
        GUARDED_MENUBAR.with(|m| *m.borrow_mut() = None);
    }

    /// Hide whatever is open and leave nothing marked open.
    fn close_open_menu(open: &OpenMenu) {
        remove_menu_key_guard();
        let previous = open.borrow_mut().take();
        let Some((toggle, dropdown)) = previous else {
            return;
        };
        if let Some(html) = dropdown.dyn_ref::<HtmlElement>() {
            let _ = html.style().set_property("display", "none");
        }
        if let Some(html) = toggle.dyn_ref::<HtmlElement>() {
            let _ = html.style().set_property("background", "transparent");
        }
    }

    fn set_css(elem: &Element, prop: &str, val: &str) {
        if let Some(html) = elem.dyn_ref::<HtmlElement>() {
            html.style().set_property(prop, val).ok();
        }
    }

    /// The GDK modifier mask for a mouse or key event: 1 = Shift, 4 =
    /// Control, 8 = Alt/Meta.
    ///
    /// Every callback in this adapter promises a GDK-style mask, because that
    /// is what the shared GUI already branches on. Translating once here is
    /// what keeps `on_motion`, `on_click_button`, `on_release` and
    /// `Window::on_event_key` agreeing -- previously `on_event_key` reported
    /// only Alt and hard-coded 0, so a caller checking Ctrl or Shift saw
    /// "no modifier" for half its cases.
    fn gdk_modifier_mask(evt: &KeyboardEvent) -> u32 {
        let mut state = 0u32;
        if evt.shift_key() {
            state |= 1;
        }
        if evt.ctrl_key() {
            state |= 4;
        }
        if evt.alt_key() {
            state |= 8;
        }
        state
    }

    /// `gdk_modifier_mask` for a mouse event. Separate because
    /// `KeyboardEvent` and `MouseEvent` are distinct wasm-bindgen types with
    /// the same three accessors.
    fn gdk_modifier_mask_mouse(evt: &MouseEvent) -> u32 {
        let mut state = 0u32;
        if evt.shift_key() {
            state |= 1;
        }
        if evt.ctrl_key() {
            state |= 4;
        }
        if evt.alt_key() {
            state |= 8;
        }
        state
    }

    /// A DOM `MouseEvent::button` number as a `core::Button`.
    ///
    /// The DOM numbering is 0 = primary (left), 1 = secondary (right),
    /// 2 = middle, 3 = back, 4 = forward, and higher for extra buttons.
    /// `core::Button` groups 0/1/2 to the same three and passes the rest
    /// through as `Other(n)` — so a pen's eraser end and a thumb button on a
    /// trackpad arrive as themselves rather than being flattened into
    /// "secondary", which is what a two-value mapping would do.
    fn button_from_dom(button: i16) -> crate::core::Button {
        use crate::core::Button;
        match button {
            0 => Button::Primary,
            1 => Button::Secondary,
            2 => Button::Middle,
            other => Button::Other(other.clamp(0, u8::MAX as i16) as u8),
        }
    }

    /// All four modifiers from a pointer event, for `core::Modifiers`.
    ///
    /// The DOM gives `meta` (Command / Windows / Menu) as its own field, where
    /// GDK folds it into one modifier bit — so reading it separately is what
    /// keeps a ⌘-click distinguishable from a Ctrl-click on the same platform.
    fn modifiers_from(evt: &web_sys::PointerEvent) -> crate::core::Modifiers {
        crate::core::Modifiers {
            shift: evt.shift_key(),
            ctrl: evt.ctrl_key(),
            alt: evt.alt_key(),
            meta: evt.meta_key(),
        }
    }

    /// Own a `Closure` for the life of the program.
    ///
    /// A `Closure` that is dropped releases the JS function it points at, so
    /// an event listener still attached to an element would call into freed
    /// memory. The three call sites that need this (`Entry::on_key_raw`,
    /// `Canvas::on_click`, `Canvas::on_key`) cannot store the closure in the
    /// widget: they are `&self` methods on a type whose other closures live in
    /// a per-widget cell that a `Clone` would share, and a listener outlives
    /// any one handle. Leaking is the correct trade here -- the page's own
    /// lifetime bounds it, and it is what `closure.forget()` does elsewhere
    /// in this file (the menu bar's click handlers).
    fn store_closure(closure: Box<dyn Any>) {
        std::mem::forget(closure);
    }

    /// The printable character a DOM `keyCode` names, or `None` for a
    /// non-printable key.
    ///
    /// The `key::` constants in `core.rs` are raw DOM `keyCode` values on
    /// this target (that is the `#[cfg(target_arch = "wasm32")] mod plat`
    /// arm), so a mnemonic is matched by mapping the code back to a letter.
    /// Only the codes a mnemonic can be typed with are handled: A-Z map to
    /// 0x41-0x5A, and 0-9 to 0x30-0x39. Anything else (arrows, modifiers,
    /// punctuation) is not a mnemonic, and returning `None` is what makes
    /// the menu fall through to normal key handling.
    fn char_from_keyval(keyval: u32) -> Option<char> {
        let c = char::from_u32(keyval)?;
        if c.is_ascii_alphanumeric() {
            Some(c)
        } else {
            None
        }
    }

    /// The printable character a key event types, or `'\0'` for a key that
    /// types nothing.
    ///
    /// `key()` is the browser's own answer: `"f"`, `"F"`, `"Enter"`, `"Alt"`,
    /// `"Dead"`. A single-character `key` is a character; everything else is a
    /// key name, and only `Enter` and `Escape` are routed (as the control
    /// characters the menu code compares against). This is what makes
    /// mnemonics work with a non-US layout: `char_from_keyval` maps
    /// `key_code` to a letter, and `key_code` is the *physical* key, so on a
    /// QWERTZ keyboard the key marked `Z` reports keyCode 89 (`Y`).
    fn typed_char(evt: &KeyboardEvent) -> char {
        let key = evt.key();
        let mut chars = key.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => c,
            _ => match key.as_str() {
                "Enter" => '\r',
                "Escape" => '\u{1b}',
                _ => '\0',
            },
        }
    }

    /// The text of a label as a reader sees it: marker and underline markup
    /// removed, `_File` -> "File" and `<u>F</u>ile` -> "File".
    fn strip_marker(label: &str) -> String {
        label
            .chars()
            .filter(|c| *c != '_' && *c != '<' && *c != '>')
            .filter(|c| *c != '/')
            .collect()
    }

    /// The letter a label marks for `Alt`, if it marks one.
    ///
    /// Two label spellings reach this, because two producers feed the menu
    /// tree:
    ///
    /// * the shared GTK model's `_File` / `S_ave as` -- the character after
    ///   the underscore;
    /// * `menu::dom_menu_label`'s rendered markup, `<u>F</u>ile` -- the
    ///   underlined character carries no underscore at all, so it is read
    ///   back out of the tag. Reading the marker of a markup label as its
    ///   first character (the previous behaviour) made `Alt+O` ("Open file")
    ///   open *Format*, whose label happens to spell out `...O...>Format<`.
    fn marked_letter(label: &str) -> Option<char> {
        if let Some(rest) = label.strip_prefix("<u>") {
            return rest.chars().next();
        }
        let i = label.find('_')?;
        label[i + 1..].chars().next()
    }

    /// The words a label is addressed by: the label with marker and markup
    /// removed, split on whitespace.
    fn label_words(label: &str) -> Vec<String> {
        strip_marker(label)
            .split_whitespace()
            .map(|w| w.to_ascii_lowercase())
            .collect()
    }

    /// Whether `ch` addresses a label.
    ///
    /// Two rules, in the order a user would try them:
    ///
    /// 1. the marked letter -- the underlined character the label
    ///    advertises (`_File` -> `F`, `S_ave as` -> `A`);
    /// 2. with `name_letters` set (top-level entries only): any letter of any
    ///    word in the entry's name. The model marks one letter per label and
    ///    its choice is not the user's -- `Format` is declared with the root
    ///    shortcut `R` (`mnemonic_label("Format", "R")`), so the letter the
    ///    entry advertises is not one a user would guess, and the plain
    ///    initial of a name is the first letter a user tries.
    ///
    /// Items are matched by rule 1 only: their labels come from a table whose
    /// shortcut column *is* the mark (`"Save as" => SaveAs ("A")` ->
    /// `S_ave as`), and `"Sort view" ("S")` -> `_Sort view` marks exactly the
    /// same `S` that `Sheet`'s marked letter does. Inside the open submenu a
    /// name match would therefore have to pick between `Save as` and `Sort
    /// view` by declaration order -- a keystroke that fires "Sort view"'s
    /// prompt out of the Sheet menu, which is what the word matching in this
    /// function did before it was restricted to the bar.

    /// The marked letter is checked first, and `activate_submenu_by_mnemonic`
    /// prefers marked matches over name matches, so an entry's own underline
    /// is never overridden by another entry's name.
    fn label_matches_mnemonic(label: &str, ch: char, name_letters: bool) -> bool {
        let ch = ch.to_ascii_lowercase();
        if marked_letter(label)
            .map(|c| c.to_ascii_lowercase() == ch)
            .unwrap_or(false)
        {
            return true;
        }
        if !name_letters {
            return false;
        }
        label_words(label)
            .iter()
            .any(|word| word.contains(ch))
    }

    /// UTF-16 code-unit offset -> character index, for the DOM's
    /// `selectionStart` / `setSelectionRange` and `Entry::get_position`.
    ///
    /// The DOM counts UTF-16 code units; the shared `Entry` API speaks
    /// character indices. They diverge for anything outside the BMP, so
    /// passing the raw value through would mis-place the caret on a cell
    /// containing an emoji.
    fn utf16_offset_to_char_index(text: &str, utf16_offset: i32) -> usize {
        let mut units = 0i32;
        let mut chars = 0usize;
        for c in text.chars() {
            if units >= utf16_offset {
                break;
            }
            units += c.len_utf16() as i32;
            chars += 1;
        }
        chars
    }

    /// The inverse of [`utf16_offset_to_char_index`].
    fn char_index_to_utf16_offset(text: &str, char_index: usize) -> i32 {
        let mut units = 0i32;
        let mut seen = 0usize;
        for c in text.chars() {
            if seen >= char_index {
                break;
            }
            units += c.len_utf16() as i32;
            seen += 1;
        }
        units
    }

    // -----------------------------------------------------------------------
    // AsElement trait – unified way to get a DOM Element from any widget
    // -----------------------------------------------------------------------
    pub trait AsElement {
        fn as_element(&self) -> &Element;
    }

    // -----------------------------------------------------------------------
    // Action registry (single‑threaded WASM, safe under wasm32)
    // -----------------------------------------------------------------------
    struct SendFnPtr(*mut dyn FnMut(*mut c_void));
    unsafe impl Send for SendFnPtr {}

    /// The one registry every function below refers to. It is a module-level
    /// item, not a `static` inside each function: `static`s declared in a
    /// function body are separate objects, so `register_action` filling one and
    /// `invoke_action` reading another made every action a silent no-op -- the
    /// menu clicked, and nothing happened.
    static ACTION_REGISTRY: once_cell::sync::Lazy<std::sync::Mutex<HashMap<String, SendFnPtr>>> =
        once_cell::sync::Lazy::new(|| std::sync::Mutex::new(HashMap::new()));

    fn register_action(name: &str, f: Box<dyn FnMut(*mut c_void)>) {
        let mut map = ACTION_REGISTRY.lock().unwrap();
        let ptr = Box::into_raw(Box::new(f) as Box<dyn FnMut(*mut c_void)>);
        // `gtk_application_set_accels_for_action` / `CreateAcceleratorTable`
        // style: the registry is keyed by the *qualified* name. GTK's group
        // name comes from `insert_action_group("app", ..)` and NWG's names are
        // already `app.*`; here the registry is flat, so the prefix is added on
        // the way in -- and `invoke_action` tries both spellings, because the
        // menu model stores the action name without it.
        let key = if name.starts_with("app.") { name.to_owned() } else { format!("app.{name}") };
        let old = map.insert(key, SendFnPtr(ptr));
        if let Some(SendFnPtr(previous)) = old {
            // Re-registering a name replaced the callback; the old box would
            // otherwise leak on every rebuild of the menu.
            drop(unsafe { Box::from_raw(previous) });
        }
    }

    /// Activate a registered action by name, as one of its widgets would.
    ///
    /// The browser-side counterpart of `gtk_widget_activate` / NWG's
    /// `SendMessage(BM_CLICK)`: the same dispatch a menu item's own handler
    /// runs, reachable without an element to click. The browser smoke test
    /// (`scripts/wasm_smoke.mjs`) needs exactly that, because which element
    /// stands for a given menu item depends on the menu's keyboard state --
    /// the top-level entries move between the bar and their dropdowns.
    fn activate_action(name: &str) -> bool {
        let known = ACTION_REGISTRY
            .lock()
            .map(|m| m.contains_key(name) || m.contains_key(&format!("app.{name}")))
            .unwrap_or(false);
        if known {
            invoke_action(name, std::ptr::null_mut());
        }
        known
    }

    fn invoke_action(name: &str, param: *mut c_void) {
        if let Ok(mut map) = ACTION_REGISTRY.lock() {
            let qualified;
            let key = if map.contains_key(name) {
                name
            } else {
                qualified = format!("app.{name}");
                &qualified
            };
            if let Some(SendFnPtr(ptr)) = map.get_mut(key) {
                let cb: &mut dyn FnMut(*mut c_void) = unsafe { &mut **ptr };
                cb(param);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Orientation
    // -----------------------------------------------------------------------
    pub enum Orientation {
        Horizontal,
        Vertical,
    }

    impl Orientation {
        pub fn as_flex_direction(&self) -> &'static str {
            match self {
                Orientation::Horizontal => "row",
                Orientation::Vertical => "column",
            }
        }
    }

    // -----------------------------------------------------------------------
    // Window
    // -----------------------------------------------------------------------
pub struct Window {
    elem: HtmlDivElement,
    event_key_cb: Rc<RefCell<Option<Box<dyn FnMut(u32, u32) -> i32>>>>,
    close_cb: Rc<RefCell<Option<Box<dyn FnMut()>>>>,
    closures: Rc<RefCell<Vec<Box<dyn Any>>>>,
}

impl Clone for Window {
    fn clone(&self) -> Self {
        Window {
            elem: self.elem.clone(),
            event_key_cb: self.event_key_cb.clone(),
            close_cb: self.close_cb.clone(),
            closures: self.closures.clone(),
        }
    }
}

impl AsElement for Window {
        fn as_element(&self) -> &Element {
            self.elem.as_ref()
        }
    }

    impl Widget for Window {
        fn raw_handle(&self) -> *mut c_void {
            &self.elem as *const HtmlDivElement as *mut c_void
        }
    }

    impl AsRef<*mut c_void> for Window {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.raw_handle() as *const *mut c_void) }
        }
    }

impl Window {
    pub fn set_title(&self, title: &str) {
        self.elem.set_text_content(Some(title));
    }

        pub fn set_child(&self, child: &impl AsElement) {
            while let Some(c) = self.elem.first_child() {
                self.elem.remove_child(&c).ok();
            }
            self.elem.append_child(child.as_element()).ok();
        }

        /// Make the window's root fill the page.
        ///
        /// `index.html` gives the document a viewport-high `body` (see the
        /// page itself); stretching the root to it is what makes a `flex: 1`
        /// child -- the sheet's row -- fill the screen instead of collapsing
        /// to its content height. Done here rather than only in the page so
        /// that a host which pastes the app into its own article still gets a
        /// full-height window.
        fn fill_viewport(&self) {
            set_css(self.elem.as_ref(), "display", "flex");
            set_css(self.elem.as_ref(), "flex-direction", "column");
            set_css(self.elem.as_ref(), "box-sizing", "border-box");
            set_css(self.elem.as_ref(), "height", "100%");
        }

        pub fn present(&self) {
            self.fill_viewport();
            // A browser paints on its own; the only work `present` has to do
            // beyond the size is to give the keyboard somewhere to go. The
            // app calls `grab_focus` on the formula entry, so this is a
            // fallback for a window that never did.
            if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
                let body: Option<Element> = doc.body().map(|b| b.into());
                let active = doc.active_element();
                if active.is_none() || active == body {
                    // Not focusable by default, so make it so: a window with
                    // nothing focused drops the first keypress into the body.
                    self.elem.set_tab_index(0);
                    let _ = self.elem.focus();
                }
            }
        }

        /// The size the requested window size is clipped to.
        ///
        /// A desktop backend is *asked* for 1200x800 and a window manager may
        /// grant it, shrink it, or move it onto a smaller monitor. A page has
        /// no window manager: a root forced to 1200x800 on a 800x600 viewport
        /// makes the page scroll and puts the status line (and any dialog)
        /// below the fold. Clipping to the viewport is the smallest of the
        /// available answers -- scrollbars (which shift the canvas under the
        /// pointer) and a hidden root (which draws nothing) are the others.
        ///
        /// The `min` is applied to the *request*, not to a hard minimum, so a
        /// caller that asks for less than the viewport (a phone-sized build on
        /// a desktop) still gets what it asked for.
        fn clipped_to_viewport(w: i32, h: i32) -> (i32, i32) {
            let (vw, vh) = web_sys::window()
                .map(|win| {
                    (
                        win.inner_width().ok().and_then(|v| v.as_f64()).unwrap_or(f64::INFINITY),
                        win.inner_height().ok().and_then(|v| v.as_f64()).unwrap_or(f64::INFINITY),
                    )
                })
                .unwrap_or((f64::INFINITY, f64::INFINITY));
            // 1px of slack: a fractional viewport (125% zoom) rounds up in
            // `inner_width` and down in layout, and a 1px overflow is a
            // scrollbar.
            let vw = vw.floor() - 1.0;
            let vh = vh.floor() - 1.0;
            let w = if w > 0 { (w as f64).min(vw).max(1.0) as i32 } else { 0 };
            let h = if h > 0 { (h as f64).min(vh).max(1.0) as i32 } else { 0 };
            (w, h)
        }

        pub fn set_default_size(&self, w: i32, h: i32) {
            self.fill_viewport();
            let (w, h) = Self::clipped_to_viewport(w, h);
            if w > 0 {
                set_css(self.elem.as_ref(), "width", &format!("{}px", w));
            }
            if h > 0 {
                set_css(self.elem.as_ref(), "height", &format!("{}px", h));
            }
        }

        /// Force the size, the way GTK's `gtk_window_resize` does.
        ///
        /// A separate method from `set_default_size` because CSS sizes are
        /// advisory in exactly the way the `common.rs` doc comment warns
        /// about: a `width` set on a flex child is a *request*, and the layout
        /// may shrink it to nothing. Setting `min-width`/`min-height` as well
        /// is what actually holds it, and it is the DOM's version of
        /// `gtk_window_set_size` (not `set_default_size`).
        ///
        /// Clipped to the viewport for the reason given on
        /// [`Self::clipped_to_viewport`]. Unlike `set_default_size` it does
        /// *not* call `fill_viewport`, because the one caller that forces a
        /// size (a movie window sized to its frame) wants exactly that size,
        /// not "the page, clipped".
        pub fn resize(&self, w: i32, h: i32) {
            let (w, h) = Self::clipped_to_viewport(w, h);
            if w > 0 {
                set_css(self.elem.as_ref(), "min-width", &format!("{}px", w));
                set_css(self.elem.as_ref(), "width", &format!("{}px", w));
            }
            if h > 0 {
                set_css(self.elem.as_ref(), "min-height", &format!("{}px", h));
                set_css(self.elem.as_ref(), "height", &format!("{}px", h));
            }
        }

        /// No OS window handle exists in a browser. `raw_handle` already
        /// returns the DOM element pointer for interop, which is the honest
        /// analogue; this is the `hwnd` slot `common.rs` requires, so it
        /// reports "no native handle" the way the ios/macos/zork adapters do.
        pub fn hwnd(&self) -> *mut c_void {
            std::ptr::null_mut()
        }

        /// `common.rs`'s `set_child_box` takes a `WidgetBox`; the adapter's
        /// `set_child` is generic over `AsElement`, so a box forwards
        /// straight through.
        pub fn set_child_box(&self, bx: &BoxWidget) {
            self.set_child(bx);
        }

        /// # Safety – kept for API compatibility; no‑op on WASM.
        pub unsafe fn insert_action_group(&self, _name: &str, _group_ptr: *mut c_void) {}
        pub fn on_event(&self, _cb: Box<dyn FnMut(*mut c_void) -> i32>) {}

        /// Fire `cb` when the page or window is closing.
        ///
        /// GTK's `close-request` and NWG's `WM_CLOSE` both mean "the user is
        /// about to lose the app", and the browser has exactly one such
        /// notification: `beforeunload`, on the *window* (it is the only
        /// unload hook that is not the end of the page). It cannot cancel
        /// the unload without a user-gesture-registered handler, so this
        /// reports rather than prevents -- the same "tell the app, let it
        /// save" contract `quit_main_loop` already uses.
        /// The window-level key fallback, the counterpart of GTK's
        /// `EventControllerKey` on the toplevel and NWG's `WM_KEYDOWN` at the
        /// frame: a key that no focused child claimed still arrives here, on
        /// `document` so that it fires wherever focus happens to be. The
        /// callback returns non-zero to consume the key.
        pub fn on_event_key(&self, cb: Box<dyn FnMut(u32, u32) -> i32>) {
            *self.event_key_cb.borrow_mut() = Some(cb);
            let cb2 = self.event_key_cb.clone();
            let closure = Closure::<dyn FnMut(KeyboardEvent)>::new(move |evt: KeyboardEvent| {
                let keyval = evt.key_code() as u32;
                // The full GDK mask, not just Alt: a caller branching on Ctrl
                // (the standard accelerator modifier) saw "no modifier" for
                // every Ctrl keypress, because only alt_key was read.
                let state = gdk_modifier_mask(&evt);
                if let Some(cb) = cb2.borrow_mut().as_mut() {
                    if cb(keyval, state) != 0 {
                        evt.prevent_default();
                    }
                }
            });
            let listener = wasm_bindgen::JsCast::unchecked_into::<js_sys::Function>(
                closure.as_ref().clone(),
            );
            web_sys::window()
                .and_then(|w| w.document())
                .map(|doc| {
                    doc.add_event_listener_with_callback("keydown", &listener).ok();
                });
            self.closures.borrow_mut().push(Box::new(closure));
        }

        /// Re-run the DOM work that a native backend would do in a redraw.
        ///
        /// GTK calls `queue_draw` on the widget tree and NWG
        /// `RedrawWindow(RD_ALLCHILDREN)`; neither has a DOM equivalent,
        /// because a browser repaints on its own. The closest honest answer
        /// is to invalidate the window's own box model, which is what makes a
        /// caller that has changed layout see the result. The child canvases
        /// are the caller's to redraw: they own their own `queue_redraw`.
        pub fn queue_redraw(&self) {
            // Reading offsetWidth/Height flushes pending style and layout, so
            // anything the caller changed has been measured by the time this
            // returns -- the same guarantee `gtk_widget_queue_draw` offers
            // for the frame that follows.
            let _ = (self.elem.offset_width(), self.elem.offset_height());
        }

        pub fn on_close(&self, cb: Box<dyn FnMut()>) {
            *self.close_cb.borrow_mut() = Some(cb);
            let cell = self.close_cb.clone();
            let closure = Closure::<dyn FnMut(Event)>::new(move |_evt: Event| {
                if let Some(f) = cell.borrow_mut().as_mut() {
                    f();
                }
            });
            if let Some(win) = web_sys::window() {
                let _ = win.add_event_listener_with_callback("beforeunload", closure.as_ref().unchecked_ref());
            }
            self.closures.borrow_mut().push(Box::new(closure));
        }

        /// Close the window, as the browser understands it: remove it from the
        /// document.
        ///
        /// `quit_main_loop` also tries `window.close()`, which a browser
        /// refuses for a tab the script did not open -- so without this, "Exit"
        /// leaves a full set of menus and a status line on screen that still
        /// look interactive but are backed by an app that has already tidied
        /// up. Removing the root makes the quit visible, and leaves the page
        /// (and `document.title`, which a test polls) intact.
        pub fn hide(&self) {
            let _ = self.elem.remove();
        }

        /// Re-run the DOM work that a native backend would do in a redraw.
        ///
        /// GTK calls `queue_draw` on the widget tree and NWG
        /// `RedrawWindow(RDW_ALLCHILDREN)`; neither has a DOM equivalent,
        /// because a browser repaints on its own. The closest honest answer
        /// is to invalidate the window's own box model, which is what makes a
        /// caller that has changed layout see the result. The child canvases
        /// are the caller's to redraw: they own their own `queue_redraw`.
        pub fn set_size_request(&self, w: i32, h: i32) {
            if w > 0 {
                set_css(self.elem.as_ref(), "width", &format!("{}px", w));
            }
            if h > 0 {
                set_css(self.elem.as_ref(), "height", &format!("{}px", h));
            }
        }

        /// `font-weight` / `font-style`, GTK `set_font_style`'s
        /// `(weight, italic)` pair.
        pub fn set_font_style(&self, weight: i32, italic: bool) {
            set_css(
                self.elem.as_ref(),
                "font-weight",
                if weight != 0 { "bold" } else { "normal" },
            );
            set_css(
                self.elem.as_ref(),
                "font-style",
                if italic { "italic" } else { "normal" },
            );
        }

        /// `classList`, the same mechanism [`Label::add_class`] uses.
        pub fn add_class(&self, class_name: &str) {
            self.elem.class_list().add_1(class_name).ok();
        }

        /// See [`Button::add_class`].
        pub fn remove_class(&self, class_name: &str) {
            self.elem.class_list().remove_1(class_name).ok();
        }
    }

    pub fn create_window() -> Result<Window, Error> {
        let div: HtmlDivElement = create_element("div").dyn_into().map_err(|e| {
            Error::Backend(format!("create_window: {:?}", e))
        })?;
        set_css(div.as_ref(), "all", "initial");
        body().append_child(div.as_ref()).map_err(|e| {
            Error::Backend(format!("create_window append: {:?}", e))
        })?;
        // Publish the action registry to the page: `window.__corro_activate_action(name)`
        // runs the same dispatch a menu item's click handler runs, and answers
        // whether the name was registered. This is the browser's stand-in for
        // the automation hook GTK and NWG get from their widget trees, and it is
        // what the smoke test uses instead of depending on DOM layout.
        if let Some(win) = web_sys::window() {
            let f = Closure::<dyn FnMut(String) -> bool>::new(|name: String| activate_action(&name));
            let _ = js_sys::Reflect::set(
                &win,
                &wasm_bindgen::JsValue::from_str("__corro_activate_action"),
                f.as_ref(),
            );
            f.forget();
        }
        Ok(Window {
            elem: div,
            event_key_cb: Rc::new(RefCell::new(None)),
            close_cb: Rc::new(RefCell::new(None)),
            closures: Rc::new(RefCell::new(Vec::new())),
        })
    }

    // -----------------------------------------------------------------------
    // Button
    // -----------------------------------------------------------------------
    pub struct Button {
        elem: HtmlButtonElement,
        click_cb: Rc<RefCell<Option<Box<dyn FnMut()>>>>,
        closures: Rc<RefCell<Vec<Box<dyn Any>>>>,
        next_id: Rc<RefCell<u64>>,
    }

    impl AsElement for Button {
        fn as_element(&self) -> &Element {
            self.elem.as_ref()
        }
    }

    impl Widget for Button {
        fn raw_handle(&self) -> *mut c_void {
            &self.elem as *const HtmlButtonElement as *mut c_void
        }
    }

    impl AsRef<*mut c_void> for Button {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.raw_handle() as *const *mut c_void) }
        }
    }

    impl Clone for Button {
        fn clone(&self) -> Self {
            Button {
                elem: self.elem.clone(),
                click_cb: self.click_cb.clone(),
                closures: self.closures.clone(),
                next_id: self.next_id.clone(),
            }
        }
    }

    impl Button {
        pub fn set_label(&self, label: &str) {
            self.elem.set_text_content(Some(label));
        }

        pub fn get_label(&self) -> Option<String> {
            self.elem.text_content()
        }

        pub fn on_click(&self, cb: Box<dyn FnMut()>) {
            *self.click_cb.borrow_mut() = Some(cb);
            let cell = self.click_cb.clone();
            let closure = Closure::<dyn FnMut(MouseEvent)>::new(move |_evt: MouseEvent| {
                if let Some(f) = cell.borrow_mut().as_mut() {
                    f();
                }
            });
            self.elem
                .add_event_listener_with_callback("click", closure.as_ref().unchecked_ref())
                .ok();
            self.closures.borrow_mut().push(Box::new(closure));
        }

        /// Programmatic activation, as `gtk_button_clicked` / `BM_CLICK` do.
        ///
        /// `elem.click()` rather than a call to the stored callback: that is
        /// what a user's click does, so it also runs whatever else is listening
        /// (a checkbox's default toggle, a form's `submit`), and it fires the
        /// same path a test or a movie replay driver expects.
        pub fn emit_clicked(&self) -> Result<(), Error> {
            self.elem.click();
            Ok(())
        }
    }

    pub fn create_button(label: &str) -> Result<Button, Error> {
        let btn: HtmlButtonElement = create_element("button").dyn_into().map_err(|e| {
            Error::Backend(format!("create_button: {:?}", e))
        })?;
        btn.set_text_content(Some(label));
        Ok(Button {
            elem: btn,
            click_cb: Rc::new(RefCell::new(None)),
            closures: Rc::new(RefCell::new(Vec::new())),
            next_id: Rc::new(RefCell::new(1)),
        })
    }

    // -----------------------------------------------------------------------
    // Label
    // -----------------------------------------------------------------------
    pub struct Label {
        elem: Element,
    }

    impl AsElement for Label {
        fn as_element(&self) -> &Element {
            &self.elem
        }
    }

    impl Clone for Label {
        fn clone(&self) -> Self {
            Label {
                elem: self.elem.clone(),
            }
        }
    }

    impl Widget for Label {
        fn raw_handle(&self) -> *mut c_void {
            &self.elem as *const Element as *mut c_void
        }
    }

    impl AsRef<*mut c_void> for Label {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.raw_handle() as *const *mut c_void) }
        }
    }

    impl Label {
        pub fn set_text(&self, text: &str) {
            self.elem.set_text_content(Some(text));
        }

        pub fn get_text(&self) -> Option<String> {
            self.elem.text_content()
        }

        pub fn add_class(&self, class_name: &str) {
            self.elem.class_list().add_1(class_name).ok();
        }

        pub fn remove_class(&self, class_name: &str) {
            self.elem.class_list().remove_1(class_name).ok();
        }

        pub fn set_markup(&self, markup: &str) {
            self.elem.set_inner_html(markup);
        }

        pub fn set_visible(&self, visible: bool) {
            if let Some(html) = self.elem.dyn_ref::<HtmlElement>() {
                if visible {
                    html.style().set_property("display", "").ok();
                } else {
                    html.style().set_property("display", "none").ok();
                }
            }
        }

        pub fn set_xalign(&self, x: f32) {
            let align = if x <= 0.0 {
                "left"
            } else if x >= 1.0 {
                "right"
            } else {
                "center"
            };
            if let Some(html) = self.elem.dyn_ref::<HtmlElement>() {
                html.style().set_property("text-align", align).ok();
            }
        }

        /// Pin the label's width so changing its text cannot resize it.
        /// `None` releases the pin.
        ///
        /// This is `width` + `min-width` together, not `width` alone: in a
        /// flex row a `width` is only a request and the layout may shrink it,
        /// while `min-width` is the floor that actually holds the slot still.
        /// Corro pins the formula-bar address and status labels for exactly
        /// this reason (`gui_backend.rs:5732` and 5768), and a `width` alone
        /// would let the sibling controls still slide as the text changes.
        pub fn set_fixed_width(&self, w: Option<i32>) {
            match w {
                Some(px) if px > 0 => {
                    let v = format!("{}px", px);
                    set_css(&self.elem, "width", &v);
                    set_css(&self.elem, "min-width", &v);
                    // A pinned slot must not be flexible, or the flex layout
                    // can still stretch or shrink it past the pin.
                    set_css(&self.elem, "flex", "0 0 auto");
                }
                // `None` clears the pin, so both properties have to go: a
                // leftover `min-width` would keep the label from ever
                // shrinking again.
                _ => {
                    set_css(&self.elem, "width", "");
                    set_css(&self.elem, "min-width", "");
                    set_css(&self.elem, "flex", "");
                }
            }
        }

        /// Left outer margin, in device px.
        ///
        /// `margin-left`, not `padding-left`: GTK's `set_margin_start` is an
        /// *outer* spacing that pushes siblings along, and corro relies on
        /// that (it pairs the inset with a pinned slot, so the inset must not
        /// eat into the pinned width).
        pub fn set_margin_start(&self, px: i32) {
            set_css(&self.elem, "margin-left", &format!("{}px", px));
        }

        /// Top outer margin, in device px.
        pub fn set_margin_top(&self, px: i32) {
            set_css(&self.elem, "margin-top", &format!("{}px", px));
        }

        /// See [`Entry::set_hexpand`].
        pub fn set_hexpand(&self, expand: bool) {
            if expand {
                set_css(&self.elem, "flex-grow", "1");
                set_css(&self.elem, "align-self", "stretch");
            } else {
                set_css(&self.elem, "flex-grow", "0");
            }
        }

        /// See [`Entry::set_vexpand`].
        pub fn set_vexpand(&self, expand: bool) {
            if expand {
                set_css(&self.elem, "flex-shrink", "1");
                set_css(&self.elem, "align-self", "stretch");
            } else {
                set_css(&self.elem, "flex-shrink", "0");
            }
        }

        pub fn set_size_request(&self, w: i32, h: i32) {
            if w > 0 {
                set_css(&self.elem, "width", &format!("{}px", w));
            }
            if h > 0 {
                set_css(&self.elem, "height", &format!("{}px", h));
            }
        }

        /// Horizontal alignment, GTK `GtkAlign` ordinals (0 = start,
        /// 1 = centre, 2 = end). Distinct from `set_xalign`, which aligns the
        /// *text* inside the label; this moves the label itself inside its
        /// slot.
        pub fn set_halign(&self, align: i32) {
            let v = match align {
                1 => "center",
                2 => "flex-end",
                _ => "flex-start",
            };
            set_css(&self.elem, "justify-self", v);
        }

        /// Vertical alignment, same ordinals as [`Label::set_halign`].
        pub fn set_valign(&self, align: i32) {
            let v = match align {
                1 => "center",
                2 => "flex-end",
                _ => "flex-start",
            };
            set_css(&self.elem, "align-self", v);
        }

        /// `AsRef` in the *other* direction: a raw handle to the DOM element,
        /// the escape hatch `common::Label::raw_handle` forwards. The DOM
        /// handle is the pointer, so this is the same value `crate::core::
        /// Widget::raw_handle` returns; it exists as an inherent method
        /// because `common.rs` calls it on the concrete type.
        pub fn raw_handle(&self) -> *mut c_void {
            &self.elem as *const Element as *mut c_void
        }
    }

    pub fn create_label(text: &str) -> Result<Label, Error> {
        let elem = create_element("span");
        elem.set_text_content(Some(text));
        Ok(Label { elem })
    }

    // -----------------------------------------------------------------------
    // BoxWidget
    // -----------------------------------------------------------------------
pub struct BoxWidget {
    elem: HtmlDivElement,
}

impl Clone for BoxWidget {
    fn clone(&self) -> Self {
        BoxWidget { elem: self.elem.clone() }
    }
}

impl AsElement for BoxWidget {
        fn as_element(&self) -> &Element {
            self.elem.as_ref()
        }
    }

    impl Widget for BoxWidget {
        fn raw_handle(&self) -> *mut c_void {
            &self.elem as *const HtmlDivElement as *mut c_void
        }
    }

    impl AsRef<*mut c_void> for BoxWidget {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.raw_handle() as *const *mut c_void) }
        }
    }

    fn as_element_from_ptr(ptr: *mut c_void) -> &'static Element {
        unsafe { &*(ptr as *const Element) }
    }

    fn as_html_element(ptr: *mut c_void) -> Option<&'static HtmlElement> {
        unsafe { (*(ptr as *const Element)).dyn_ref::<HtmlElement>() }
    }

    impl BoxWidget {
        pub fn append(&self, child: &impl AsRef<*mut c_void>) {
            self.elem.append_child(as_element_from_ptr(*child.as_ref())).ok();
        }

        pub fn set_child_vexpand(&self, child: &impl AsRef<*mut c_void>, expand: bool) {
            if let Some(html) = as_html_element(*child.as_ref()) {
                if expand {
                    html.style().set_property("flex-grow", "1").ok();
                    html.style().set_property("align-self", "stretch").ok();
                } else {
                    html.style().set_property("flex-grow", "0").ok();
                }
            }
        }

        pub fn set_child_hexpand(&self, child: &impl AsRef<*mut c_void>, expand: bool) {
            if let Some(html) = as_html_element(*child.as_ref()) {
                if expand {
                    html.style().set_property("align-self", "stretch").ok();
                }
            }
        }

        /// See [`Entry::set_hexpand`]: `flex-grow` is how a box takes the
        /// leftover space, and `align-self: stretch` is the cross-axis half.
        pub fn set_hexpand(&self, expand: bool) {
            if expand {
                set_css(self.elem.as_ref(), "flex-grow", "1");
                set_css(self.elem.as_ref(), "align-self", "stretch");
            } else {
                set_css(self.elem.as_ref(), "flex-grow", "0");
            }
        }

        /// See [`BoxWidget::set_hexpand`].
        pub fn set_vexpand(&self, expand: bool) {
            if expand {
                set_css(self.elem.as_ref(), "flex-grow", "1");
                set_css(self.elem.as_ref(), "align-self", "stretch");
            } else {
                set_css(self.elem.as_ref(), "flex-grow", "0");
            }
        }

        pub fn set_visible(&self, v: bool) {
            set_css(self.elem.as_ref(), "display", if v { "" } else { "none" });
        }

        pub fn set_size_request(&self, w: i32, h: i32) {
            if w > 0 {
                set_css(self.elem.as_ref(), "width", &format!("{}px", w));
            }
            if h > 0 {
                set_css(self.elem.as_ref(), "height", &format!("{}px", h));
            }
        }
    }

    pub fn create_box(orientation: Orientation, spacing: i32) -> Result<BoxWidget, Error> {
        let div: HtmlDivElement = create_element("div").dyn_into().map_err(|e| {
            Error::Backend(format!("create_box: {:?}", e))
        })?;
        let s = div.style();
        s.set_property("display", "flex").ok();
        s.set_property("flex-direction", orientation.as_flex_direction())
            .ok();
        if spacing > 0 {
            s.set_property("gap", &format!("{}px", spacing)).ok();
        }
        Ok(BoxWidget { elem: div })
    }

    // -----------------------------------------------------------------------
    // Grid
    // -----------------------------------------------------------------------
    pub struct Grid {
        elem: HtmlDivElement,
    }

    impl AsElement for Grid {
        fn as_element(&self) -> &Element {
            self.elem.as_ref()
        }
    }

    impl Widget for Grid {
        fn raw_handle(&self) -> *mut c_void {
            &self.elem as *const HtmlDivElement as *mut c_void
        }
    }

    impl Grid {
        pub fn attach(&self, child: &impl AsElement, left: i32, top: i32, width: i32, height: i32) {
            let child = child.as_element();
            if let Some(html) = child.dyn_ref::<HtmlElement>() {
                let s = html.style();
                s.set_property(
                    "grid-column",
                    &format!("{} / {}", left + 1, left + width + 1),
                )
                .ok();
                s.set_property(
                    "grid-row",
                    &format!("{} / {}", top + 1, top + height + 1),
                )
                .ok();
            }
            self.elem.append_child(child).ok();
        }

        pub fn set_visible(&self, v: bool) {
            set_css(self.elem.as_ref(), "display", if v { "" } else { "none" });
        }
        pub fn set_hexpand(&self, expand: bool) {
            if expand {
                set_css(self.elem.as_ref(), "flex-grow", "1");
                set_css(self.elem.as_ref(), "align-self", "stretch");
            } else {
                set_css(self.elem.as_ref(), "flex-grow", "0");
            }
        }
        pub fn set_vexpand(&self, expand: bool) {
            if expand {
                set_css(self.elem.as_ref(), "flex-grow", "1");
                set_css(self.elem.as_ref(), "align-self", "stretch");
            } else {
                set_css(self.elem.as_ref(), "flex-grow", "0");
            }
        }
        pub fn set_size_request(&self, w: i32, h: i32) {
            if w > 0 {
                set_css(self.elem.as_ref(), "width", &format!("{}px", w));
            }
            if h > 0 {
                set_css(self.elem.as_ref(), "height", &format!("{}px", h));
            }
        }
    }

    pub fn create_grid() -> Result<Grid, Error> {
        let div: HtmlDivElement = create_element("div").dyn_into().map_err(|e| {
            Error::Backend(format!("create_grid: {:?}", e))
        })?;
        div.style().set_property("display", "grid").ok();
        Ok(Grid { elem: div })
    }

    // -----------------------------------------------------------------------
    // Entry
    // -----------------------------------------------------------------------
    pub struct Entry {
        elem: HtmlInputElement,
        key_cb: Rc<RefCell<Option<Box<dyn FnMut(u32, u32) -> bool>>>>,
        closures: Rc<RefCell<Vec<Box<dyn Any>>>>,
        next_id: Rc<RefCell<u64>>,
    }

    impl AsElement for Entry {
        fn as_element(&self) -> &Element {
            self.elem.as_ref()
        }
    }

    impl Widget for Entry {
        fn raw_handle(&self) -> *mut c_void {
            &self.elem as *const HtmlInputElement as *mut c_void
        }
    }

    impl Clone for Entry {
        fn clone(&self) -> Self {
            Entry {
                elem: self.elem.clone(),
                key_cb: self.key_cb.clone(),
                closures: self.closures.clone(),
                next_id: self.next_id.clone(),
            }
        }
    }

    impl AsRef<*mut c_void> for Entry {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.raw_handle() as *const *mut c_void) }
        }
    }

    impl Entry {
        pub fn set_text(&self, text: &str) {
            self.elem.set_value(text);
        }

        pub fn get_text(&self) -> Option<String> {
            Some(self.elem.value())
        }


        pub fn set_width_chars(&self, n: i32) {
            self.elem.set_size(n as u32);
        }

        pub fn set_size_request(&self, w: i32, h: i32) {
            if w > 0 {
                set_css(self.elem.as_ref(), "width", &format!("{}px", w));
            }
            if h > 0 {
                set_css(self.elem.as_ref(), "height", &format!("{}px", h));
            }
        }

        pub fn set_hexpand(&self, expand: bool) {
            // `flex-grow` is the only way an input takes leftover space: an
            // <input> measures to its content, so in a bare flex row it stays
            // at whatever `size` said and the entry does not follow the window
            // being resized.
            if expand {
                set_css(self.elem.as_ref(), "flex-grow", "1");
                set_css(self.elem.as_ref(), "align-self", "stretch");
            } else {
                set_css(self.elem.as_ref(), "flex-grow", "0");
            }
        }

        /// See [`Entry::set_hexpand`]: `align-self` is the cross-axis
        /// property in the flex row a formula bar is.
        pub fn set_vexpand(&self, expand: bool) {
            if expand {
                set_css(self.elem.as_ref(), "flex-shrink", "1");
                set_css(self.elem.as_ref(), "align-self", "stretch");
            } else {
                set_css(self.elem.as_ref(), "flex-shrink", "0");
            }
        }

        pub fn set_visible(&self, v: bool) {
            set_css(self.elem.as_ref(), "display", if v { "" } else { "none" });
        }

        /// Horizontal alignment, GTK `GtkAlign` ordinals (0 = start,
        /// 1 = centre, 2 = end).
        pub fn set_halign(&self, align: i32) {
            let v = match align {
                1 => "center",
                2 => "flex-end",
                _ => "flex-start",
            };
            set_css(self.elem.as_ref(), "justify-self", v);
        }

        /// Vertical alignment, same ordinals as [`Entry::set_halign`].
        pub fn set_valign(&self, align: i32) {
            let v = match align {
                1 => "center",
                2 => "flex-end",
                _ => "flex-start",
            };
            set_css(self.elem.as_ref(), "align-self", v);
        }

        pub fn set_margin_start(&self, px: i32) {
            set_css(self.elem.as_ref(), "margin-left", &format!("{}px", px));
        }

        pub fn set_margin_top(&self, px: i32) {
            set_css(self.elem.as_ref(), "margin-top", &format!("{}px", px));
        }

        pub fn connect_changed(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            let cb = Rc::new(RefCell::new(f));
            let cb2 = cb.clone();
            let closure = Closure::<dyn FnMut(Event)>::new(move |_: Event| {
                (cb2.borrow_mut())();
            });
            self.elem
                .add_event_listener_with_callback("input", closure.as_ref().unchecked_ref())
                .map_err(|e| Error::Backend(format!("connect_changed: {:?}", e)))?;
            let id = *self.next_id.borrow();
            *self.next_id.borrow_mut() += 1;
            self.closures.borrow_mut().push(Box::new(closure));
            Ok(id)
        }

        pub fn connect_activate<F: FnMut(*mut c_void) + 'static>(
            &self,
            f: F,
        ) -> Result<u64, Error> {
            let cb = Rc::new(RefCell::new(f));
            let cb2 = cb.clone();
            let closure =
                Closure::<dyn FnMut(KeyboardEvent)>::new(move |evt: KeyboardEvent| {
                    if evt.key() == "Enter" {
                        (cb2.borrow_mut())(std::ptr::null_mut());
                    }
                });
            self.elem
                .add_event_listener_with_callback("keydown", closure.as_ref().unchecked_ref())
                .map_err(|e| Error::Backend(format!("connect_activate: {:?}", e)))?;
            let id = *self.next_id.borrow();
            *self.next_id.borrow_mut() += 1;
            self.closures.borrow_mut().push(Box::new(closure));
            Ok(id)
        }

        /// Whether the input currently holds focus.
        ///
        /// This was hard-coded `false`, and that is not a neutral default:
        /// `common::Entry::has_focus` (common.rs:238) is what callers use to
        /// choose between pushing a new value and appending to the current
        /// one, so always-false silently made every keystroke push. The DOM
        /// answer is `document.activeElement === input`.
        pub fn has_focus(&self) -> bool {
            document()
                .active_element()
                .map(|active| &active == self.elem.as_ref())
                .unwrap_or(false)
        }

        pub fn connect_button_press(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            let cb = Rc::new(RefCell::new(f));
            let cb2 = cb.clone();
            let closure = Closure::<dyn FnMut(MouseEvent)>::new(move |_: MouseEvent| {
                (cb2.borrow_mut())();
            });
            self.elem
                .add_event_listener_with_callback("click", closure.as_ref().unchecked_ref())
                .map_err(|e| Error::Backend(format!("connect_button_press: {:?}", e)))?;
            let id = *self.next_id.borrow();
            *self.next_id.borrow_mut() += 1;
            self.closures.borrow_mut().push(Box::new(closure));
            Ok(id)
        }

        pub fn add_class(&self, class_name: &str) {
            self.elem.class_list().add_1(class_name).ok();
        }

        pub fn remove_class(&self, class_name: &str) {
            self.elem.class_list().remove_1(class_name).ok();
        }

        pub fn grab_focus(&self) {
            let _ = self.elem.focus();
        }
        /// The caret offset. `common::Entry` documents this as a *character*
        /// index while the DOM's `selectionStart` counts UTF-16 code units,
        /// and the two differ for anything outside the BMP (an emoji in a
        /// cell value is enough), so the offset is converted rather than
        /// passed through.
        pub fn get_position(&self) -> Option<usize> {
            // `selection_start` is `Result<Option<u32>, JsValue>`: the inner
            // `None` is "no selection" (an input that has never been focused),
            // which is the same answer as a caret at the beginning. A `None`
            // from the outer Result is an un-focusable input, and reporting
            // no position at all is right for that.
            let start = self.elem.selection_start().ok().flatten().unwrap_or(0) as i32;
            Some(utf16_offset_to_char_index(&self.elem.value(), start))
        }

        /// Move the caret, taking a character index and converting it to the
        /// UTF-16 offset the DOM wants.
        pub fn set_position(&self, pos: usize) {
            let offset = char_index_to_utf16_offset(&self.elem.value(), pos) as u32;
            // The setters take `Option<u32>` and throw on `None`, so they are
            // wrapped in `Some` rather than unwrapped: "set the selection to
            // 0" is a real instruction, not a missing one.
            let _ = self.elem.set_selection_start(Some(offset));
            let _ = self.elem.set_selection_end(Some(offset));
        }

        pub fn on_key_raw(&self, cb: Box<dyn FnMut(u32, u32) -> bool>) {
            *self.key_cb.borrow_mut() = Some(cb);
            let cb2 = self.key_cb.clone();
            let closure = Closure::<dyn FnMut(KeyboardEvent)>::new(move |evt: KeyboardEvent| {
                if let Some(ref mut f) = *cb2.borrow_mut() {
                    if f(evt.key_code(), 0) {
                        evt.prevent_default();
                    }
                }
            });
            self.elem
                .add_event_listener_with_callback("keydown", closure.as_ref().unchecked_ref())
                .ok();
            store_closure(Box::new(closure));
        }

        pub fn connect_focus_in_event<F: FnMut(*mut c_void) -> i32 + 'static>(
            &self,
            f: F,
        ) -> Result<u64, Error> {
            let cb = Rc::new(RefCell::new(f));
            let cb2 = cb.clone();
            let closure = Closure::<dyn FnMut(FocusEvent)>::new(move |_: FocusEvent| {
                (cb2.borrow_mut())(std::ptr::null_mut());
            });
            self.elem
                .add_event_listener_with_callback("focus", closure.as_ref().unchecked_ref())
                .map_err(|e| Error::Backend(format!("connect_focus_in_event: {:?}", e)))?;
            let id = *self.next_id.borrow();
            *self.next_id.borrow_mut() += 1;
            self.closures.borrow_mut().push(Box::new(closure));
            Ok(id)
        }

        pub fn connect_focus_out_event<F: FnMut(*mut c_void) -> i32 + 'static>(
            &self,
            f: F,
        ) -> Result<u64, Error> {
            let cb = Rc::new(RefCell::new(f));
            let cb2 = cb.clone();
            let closure = Closure::<dyn FnMut(FocusEvent)>::new(move |_: FocusEvent| {
                (cb2.borrow_mut())(std::ptr::null_mut());
            });
            self.elem
                .add_event_listener_with_callback("blur", closure.as_ref().unchecked_ref())
                .map_err(|e| Error::Backend(format!("connect_focus_out_event: {:?}", e)))?;
            let id = *self.next_id.borrow();
            *self.next_id.borrow_mut() += 1;
            self.closures.borrow_mut().push(Box::new(closure));
            Ok(id)
        }
    }

    pub fn create_entry() -> Result<Entry, Error> {
        let elem: HtmlInputElement = create_element("input").dyn_into().map_err(|e| {
            Error::Backend(format!("create_entry: {:?}", e))
        })?;
        elem.set_type("text");
        Ok(Entry {
            elem,
            key_cb: Rc::new(RefCell::new(None)),
            closures: Rc::new(RefCell::new(Vec::new())),
            next_id: Rc::new(RefCell::new(1)),
        })
    }

    // -----------------------------------------------------------------------
    // Menu, MenuBar, SimpleAction
    // -----------------------------------------------------------------------
    #[derive(Clone)]
    enum MenuItem {
        Item { label: String, action: String },
        Submenu { label: String, items: Vec<MenuItem> },
    }

#[derive(Clone)]
pub struct Menu {
    items: Vec<MenuItem>,
}

impl Menu {
        pub fn append(&mut self, label: &str, detailed_action: &str) {
            let action = detailed_action
                .split('(')
                .next()
                .unwrap_or(detailed_action)
                .to_owned();
            self.items.push(MenuItem::Item {
                label: label.to_owned(),
                action,
            });
        }

        pub fn append_submenu(&mut self, label: &str, submenu: &Menu) {
            self.items.push(MenuItem::Submenu {
                label: label.to_owned(),
                items: submenu.items.clone(),
            });
        }
    }

    pub fn create_menu() -> Result<Menu, Error> {
        Ok(Menu { items: Vec::new() })
    }

pub struct MenuBar {
    elem: HtmlDivElement,
    /// The top-level menus, in order, with the DOM elements that open them.
    ///
    /// The keyboard API needs this: `Alt+F` has to find *the* submenu whose
    /// label starts with F, and the only way to know that is to keep the
    /// labels alongside the elements. GTK gets it from `GMenu`, which carries
    /// the labels itself; the DOM does not.
    entries: Rc<RefCell<Vec<MenuEntry>>>,
    /// The open submenu, if any: its toggle button and its dropdown.
    open: Rc<RefCell<Option<(Element, Element)>>>,
    /// Index of the highlighted item in the open submenu, for arrow-key
    /// navigation.
    selected: Rc<RefCell<usize>>,
    /// The active response callback, so a chosen item can be invoked.
    closures: Rc<RefCell<Vec<Box<dyn Any>>>>,
}

/// One top-level menu: its label, the button that toggles it, and the
/// dropdown it opens.
struct MenuEntry {
    label: String,
    /// The `<button>` that opens the dropdown.
    toggle: Element,
    /// The absolutely-positioned dropdown below it.
    dropdown: Element,
    /// `(label, action)` for each item, in display order. Kept so
    /// `activate_submenu_item_by_mnemonic` can dispatch by key.
    items: Vec<(String, String)>,
}

impl Clone for MenuBar {
    fn clone(&self) -> Self {
        MenuBar {
            elem: self.elem.clone(),
            entries: self.entries.clone(),
            open: self.open.clone(),
            selected: self.selected.clone(),
            closures: self.closures.clone(),
        }
    }
}

impl AsElement for MenuBar {
        fn as_element(&self) -> &Element {
            self.elem.as_ref()
        }
    }

    impl Widget for MenuBar {
        fn raw_handle(&self) -> *mut c_void {
            &self.elem as *const HtmlDivElement as *mut c_void
        }
    }

    impl AsRef<*mut c_void> for MenuBar {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.raw_handle() as *const *mut c_void) }
        }
    }

    impl MenuBar {
        /// Open the submenu whose first letter is `keyval`, and report
        /// whether one matched.
        ///
        /// The rules are in `label_matches_mnemonic`: the marked letter
        /// first, then -- for a top-level entry, where the model's marks
        /// collide (`File` and `Format` are both marked `F`) -- the letters
        /// of the entry's own name.
        pub fn activate_submenu_by_mnemonic(&self, keyval: u32) -> bool {
            let ch = match char_from_keyval(keyval) {
                Some(c) => c,
                None => return false,
            };
            self.activate_submenu_by_char(ch)
        }

        /// Open the submenu `ch` addresses (see `label_matches_mnemonic`).
        fn activate_submenu_by_char(&self, ch: char) -> bool {
            let ch = ch.to_ascii_lowercase();
            // A marked match beats a name match: the model underlines exactly
            // one letter per entry, and that underline is the accelerator the
            // label advertises (`_File` marks `F`), so `F` may never fall
            // through to Format just because File appears first. Only when no
            // entry is *marked* with the letter does the name fallback below
            // decide, which is what makes `Alt+E`/`Alt+I`/`Alt+O`/`Alt+H`
            // work when the model marked something else entirely.
            let entries = self.entries.borrow();
            let marked = entries.iter().position(|e| {
                marked_letter(&e.label)
                    .map(|c| c.to_ascii_lowercase() == ch)
                    .unwrap_or(false)
            });
            let idx = marked.or_else(|| {
                entries
                    .iter()
                    .position(|e| label_matches_mnemonic(&e.label, ch, true))
            });
            drop(entries);
            match idx {
                Some(i) => {
                    self.open_submenu(i);
                    true
                }
                None => false,
            }
        }

        /// Open the submenu whose mnemonic is `keyval` at a screen position.
        ///
        /// The dropdown is normally positioned by CSS (`position: absolute`
        /// under its toggle). A caller that wants it at the pointer -- the
        /// sheet-tab context menu, which GTK does through its
        /// position-callback -- gets it here by overriding the offsets.
        pub fn popup_submenu_by_mnemonic_at(&self, keyval: u32, screen_x: i32, screen_y: i32) -> bool {
            let ch = match char_from_keyval(keyval) {
                Some(c) => c,
                None => return false,
            };
            let entries = self.entries.borrow();
            let marked = entries.iter().position(|e| {
                marked_letter(&e.label)
                    .map(|c| c.to_ascii_lowercase() == ch.to_ascii_lowercase())
                    .unwrap_or(false)
            });
            let idx = marked.or_else(|| {
                entries
                    .iter()
                    .position(|e| label_matches_mnemonic(&e.label, ch, true))
            });
            drop(entries);
            match idx {
                Some(i) => {
                    self.open_submenu(i);
                    // The dropdown is `position: absolute` inside the bar, so
                    // an offset is relative to the bar -- which sits at the
                    // top of the page. The caller's screen coordinates are
                    // converted through the bar's own bounding box, the same
                    // way GTK's position callback receives screen coords and
                    // converts to widget coords.
                    {
                        let rect = self.elem.get_bounding_client_rect();
                        let dx = screen_x as f64 - rect.x();
                        let dy = screen_y as f64 - rect.y();
                        let entries = self.entries.borrow();
                        if let Some(entry) = entries.get(i) {
                            if let Some(html) = entry.dropdown.dyn_ref::<HtmlElement>() {
                                let _ = html.style().set_property("left", &format!("{}px", dx));
                                let _ = html.style().set_property("top", &format!("{}px", dy));
                            }
                        }
                    }
                    true
                }
                None => false,
            }
        }

        /// WASM positions the popup itself, relative to its own bar, so the
        /// only translation needed here is the anchor's on-screen origin (a
        /// browser has no screen coordinates of its own -- see
        /// `Canvas::screen_origin`).
        pub fn popup_at(&self, label: &str, anchor: &Canvas, x: f64, y: f64) -> bool {
            let keyval = match label.chars().next() {
                Some(c) => c as u32,
                None => return false,
            };
            let origin = anchor.screen_origin().unwrap_or((0, 0));
            self.popup_submenu_by_mnemonic_at(keyval, origin.0 + x as i32, origin.1 + y as i32)
        }
        /// Activate the item in the open submenu whose label starts with
        /// `keyval`. Requires a submenu to be open, which is what
        /// `menu_active` reports.
        pub fn activate_submenu_item_by_mnemonic(&self, keyval: u32) -> bool {
            match char_from_keyval(keyval) {
                Some(ch) => self.activate_submenu_item_by_char(ch),
                None => false,
            }
        }

        /// Activate the open submenu's item that `ch` marks.
        fn activate_submenu_item_by_char(&self, ch: char) -> bool {
            let open = self.open.borrow();
            let Some((_, _)) = open.as_ref() else {
                return false;
            };
            drop(open);
            // Find the item whose marked letter matches, then dispatch its
            // action. The marked letter (not the first character) is what the
            // user reads off the label: `Save as` is accelerated by `a`, which
            // the shared menu tree declares, and matching the first character
            // made every such item unreachable by its own mnemonic.
            let found = self.entries.borrow().iter().enumerate().find_map(|(entry_idx, e)| {
                e.items
                    .iter()
                    .position(|(label, _)| label_matches_mnemonic(label, ch, false))
                    .map(|item_idx| (entry_idx, item_idx))
            });
            match found {
                Some((entry_idx, item_idx)) => {
                    let action = {
                        let entries = self.entries.borrow();
                        entries[entry_idx].items[item_idx].1.clone()
                    };
                    self.menu_close();
                    invoke_action(&action, std::ptr::null_mut());
                    true
                }
                None => false,
            }
        }

        /// # Safety
        /// Kept for API compatibility; no-op (the DOM menu is wired directly
        /// to the action registry, as on NWG).
        pub unsafe fn insert_action_group(&self, _name: &str, _group_ptr: *mut c_void) {}

        /// Handle a keypress when a submenu may be open.
        ///
        /// With a submenu open: Escape closes it, a printable key activates
        /// the item it starts, and Up/Down move the selection. With nothing
        /// open this only answers Alt+letter (via
        /// `activate_submenu_by_mnemonic`) and reports `false` otherwise, so
        /// the caller falls through to normal key handling.
        pub fn handle_mnemonic_key(&self, keyval: u32) -> bool {
            if keyval == crate::core::key::ESCAPE {
                return self.handle_mnemonic_char('\u{1b}');
            }
            if keyval == crate::core::key::DOWN || keyval == crate::core::key::UP {
                return self.handle_mnemonic_char(if keyval == crate::core::key::DOWN {
                    '\u{1f}'
                } else {
                    '\u{1e}'
                });
            }
            match char_from_keyval(keyval) {
                Some(ch) => self.handle_mnemonic_char(ch),
                None => false,
            }
        }

        /// A keystroke while a submenu is open: Escape closes it, Up/Down move
        /// the selection (passed as the non-character sentinels `\u{1e}` /
        /// `\u{1f}`), and any other character activates the item it marks.
        fn handle_mnemonic_char(&self, ch: char) -> bool {
            if !self.menu_active() {
                return false;
            }
            match ch {
                '\u{1b}' => {
                    self.menu_close();
                    true
                }
                '\u{1f}' => {
                    self.move_selection(true);
                    true
                }
                '\u{1e}' => {
                    self.move_selection(false);
                    true
                }
                _ => self.activate_submenu_item_by_char(ch),
            }
        }

        /// Handle any menu-related key event.
        ///
        /// `modifiers` is the GDK mask (1 = Shift, 4 = Control, 8 = Alt), the
        /// same one `Window::on_event_key` reports. Alt+letter opens a
        /// submenu; otherwise this defers to `handle_mnemonic_key`, so the
        /// navigation rules live in one place.
        pub fn handle_menu_key(&self, keyval: u32, modifiers: u32) -> bool {
            self.handle_menu_key_char(
                keyval,
                char::from_u32(keyval).unwrap_or('\0'),
                modifiers,
            )
        }

        /// [`handle_menu_key`](Self::handle_menu_key), given the typed
        /// character as well as the key code, for a host whose key codes are
        /// not character values.
        ///
        /// A page's key codes are (A-Z map to 0x41-0x5A on a US layout), but
        /// the DOM reports the *physical* key, so on another layout the
        /// labelled letter and the code disagree; the caller that has the
        /// event passes `key()` through [`typed_char`].
        pub fn handle_menu_key_char(&self, keyval: u32, ch: char, modifiers: u32) -> bool {
            const ALT_MASK: u32 = 0x8;
            let ch = if ch == '\0' {
                char_from_keyval(keyval).unwrap_or('\0')
            } else {
                ch
            };
            if ch == '\0' {
                return false;
            }
            if modifiers & ALT_MASK != 0 && self.activate_submenu_by_char(ch) {
                return true;
            }
            self.handle_mnemonic_char(ch)
        }

        /// Whether a keyboard menu is open. Callers use this to skip normal
        /// key handling while one is.
        pub fn menu_active(&self) -> bool {
            self.open.borrow().is_some()
        }

        /// Close the open submenu, its DOM dropdown, and clear the selection.
        ///
        /// The DOM work goes through [`close_open_menu`], the same helper the
        /// click handlers use, so the bar's state and the page cannot diverge.
        pub fn menu_close(&self) {
            close_open_menu(&self.open);
            *self.selected.borrow_mut() = 0;
        }

        /// Show submenu `idx` and hide any other.
        fn open_submenu(&self, idx: usize) {
            close_open_menu(&self.open);
            let entries = self.entries.borrow();
            let Some(entry) = entries.get(idx) else {
                return;
            };
            open_menu_entry(&self.open, &entry.dropdown, Some(&entry.toggle));
            install_menu_key_guard(&self.open, self);
            *self.selected.borrow_mut() = 0;
        }

        /// Move the highlight by one item in the open submenu, clamping at
        /// the ends.
        fn move_selection(&self, forward: bool) {
            let count = {
                let open = self.open.borrow();
                let Some((_, dropdown)) = open.as_ref() else {
                    return;
                };
                dropdown.children().length() as usize
            };
            if count == 0 {
                return;
            }
            let mut sel = self.selected.borrow_mut();
            *sel = if forward {
                (*sel + 1) % count
            } else {
                (*sel + count - 1) % count
            };
            // The highlight is what a caller reads back to know where the
            // selection is, so it has to be visible: re-apply the background
            // to the selected child only.
            let open = self.open.borrow();
            if let Some((_, dropdown)) = open.as_ref() {
                for i in 0..count {
                    if let Some(child) = dropdown.children().item(i as u32) {
                        if let Some(html) = child.dyn_ref::<HtmlElement>() {
                            let _ = html.style().set_property(
                                "background",
                                if i == *sel { "#d0d8e8" } else { "" },
                            );
                        }
                    }
                }
            }
        }
    }

    pub fn create_menubar(model: &Menu, _action_group: *mut c_void) -> Result<MenuBar, Error> {
        let bar: HtmlDivElement = create_element("div").dyn_into().map_err(|e| {
            Error::Backend(format!("create_menubar: {:?}", e))
        })?;
        let s = bar.style();
        s.set_property("display", "flex").ok();
        s.set_property("background", "#f0f0f0").ok();
        s.set_property("border-bottom", "1px solid #ccc").ok();

        let mut entries: Vec<MenuEntry> = Vec::new();
        // One open-state cell for the whole bar, shared with the click
        // handlers (see `OpenMenu`).
        let open_handle: OpenMenu = Rc::new(RefCell::new(None));
        for item in &model.items {
            match item {
                MenuItem::Item { label, action } => {
                    let btn: HtmlButtonElement = create_element("button").dyn_into().unwrap();
                    set_menu_label(btn.as_ref(), label);
                    let action_name = action.clone();
                    let menu = open_handle.clone();
                    let closure =
                        Closure::<dyn FnMut(MouseEvent)>::new(move |evt: MouseEvent| {
                            // Any open submenu closes when an action runs --
                            // the same rule the dropdown items follow.
                            close_open_menu(&menu);
                            invoke_action(&action_name, std::ptr::null_mut());
                            evt.stop_propagation();
                        });
                    btn.add_event_listener_with_callback("click", closure.as_ref().unchecked_ref())
                        .ok();
                    // A menu item is not a text input: without `mousedown`
                    // prevented the browser moves focus to the button on
                    // press, and the formula entry the app put focus in loses
                    // it. The shared GUI then reports `has_focus() == false`
                    // for the entry and every shortcut typed afterwards is
                    // treated as cell text instead of as an accelerator.
                    let keep_focus = Closure::<dyn FnMut(MouseEvent)>::new(|evt: MouseEvent| {
                        // See the item handler: the press must not become a
                        // click outside, nor move focus out of the formula
                        // entry.
                        evt.stop_propagation();
                        evt.prevent_default();
                    });
                    btn.add_event_listener_with_callback("mousedown", keep_focus.as_ref().unchecked_ref())
                        .ok();
                    keep_focus.forget();
                    closure.forget();
                    let s = btn.style();
                    s.set_property("background", "transparent").ok();
                    s.set_property("border", "none").ok();
                    s.set_property("padding", "4px 12px").ok();
                    s.set_property("cursor", "pointer").ok();
                    bar.append_child(btn.as_ref()).ok();
                    // A bare top-level item is still addressable by
                    // mnemonic: it has no dropdown, so the "dropdown" is the
                    // button itself and `menu_active` stays false for it.
                    // `HtmlButtonElement` implements `AsRef` for both
                    // `Element` and `HtmlElement`, so `btn.as_ref()` is
                    // ambiguous here; the turbofish names the one wanted.
                    let elem: Element = <HtmlButtonElement as AsRef<Element>>::as_ref(&btn).clone();
                    entries.push(MenuEntry {
                        label: label.clone(),
                        toggle: elem.clone(),
                        dropdown: elem,
                        items: vec![(label.clone(), action.clone())],
                    });
                }
                MenuItem::Submenu { label, items } => {
                    let wrapper = create_element("div");
                    set_css(&wrapper, "position", "relative");

                    let toggle: HtmlButtonElement = create_element("button").dyn_into().unwrap();
                    set_menu_label(toggle.as_ref(), label);
                    let s = toggle.style();
                    s.set_property("background", "transparent").ok();
                    s.set_property("border", "none").ok();
                    s.set_property("padding", "4px 12px").ok();
                    s.set_property("cursor", "pointer").ok();

                    let dropdown = create_element("div");
                    set_css(&dropdown, "display", "none");
                    set_css(&dropdown, "position", "absolute");
                    set_css(&dropdown, "top", "100%");
                    set_css(&dropdown, "left", "0");
                    set_css(&dropdown, "background", "#fff");
                    set_css(&dropdown, "border", "1px solid #ccc");
                    set_css(&dropdown, "z-index", "1000");

                    let mut entry_items: Vec<(String, String)> = Vec::new();
                    for sub in items {
                        match sub {
                            MenuItem::Item { label, action } => {
                                let item = create_element("div");
                                set_css(&item, "padding", "4px 12px");
                                set_css(&item, "cursor", "pointer");
                                set_menu_label(&item, label);
                                let action_name = action.clone();
                                // Close first, then invoke: an action can
                                // rebuild or replace the menubar (`Exit`
                                // removes the whole window), and a menu left
                                // "open" after a click would keep a dropdown
                                // on screen over the next screen the action
                                // draws. Also what every desktop menubar does.
                                let menu = open_handle.clone();
                                let cl =
                                    Closure::<dyn FnMut(MouseEvent)>::new(move |evt: MouseEvent| {
                                        close_open_menu(&menu);
                                        invoke_action(&action_name, std::ptr::null_mut());
                                        // The menu must not swallow the click
                                        // that is closing it: the document
                                        // listener below would otherwise see
                                        // this same event and treat it as a
                                        // click outside.
                                        evt.stop_propagation();
                                    });
                                item.add_event_listener_with_callback(
                                    "click",
                                    cl.as_ref().unchecked_ref(),
                                )
                                .ok();
                                cl.forget();
                                // See the top-level menu button: pressing an
                                // item must not steal focus from the formula
                                // entry, or the app stops seeing accelerators.
                                //
                                // It must also not reach the document-level
                                // "click outside" listener, which would close
                                // the menu on *press* -- before the click that
                                // picks the item ever fires. Suppressing the
                                // focus change and stopping propagation happen
                                // together here for that reason: both are
                                // "this press belongs to the item".
                                let keep = Closure::<dyn FnMut(MouseEvent)>::new(
                                    |evt: MouseEvent| {
                                        evt.stop_propagation();
                                        evt.prevent_default();
                                    },
                                );
                                item.add_event_listener_with_callback(
                                    "mousedown",
                                    keep.as_ref().unchecked_ref(),
                                )
                                .ok();
                                keep.forget();
                                dropdown.append_child(&item).ok();
                                // Recorded so the keyboard API can dispatch by
                                // mnemonic: the DOM item has no way to tell
                                // Rust which action it stands for.
                                entry_items.push((label.clone(), action.clone()));
                            }
                            _ => {}
                        }
                    }

                    // The toggle drives the shared open-state, not just its own
                    // `display`: closing only the dropdown left the menubar
                    // believing a menu was still open, so Escape and the
                    // keyboard mnemonics kept being routed to it and normal
                    // typing was swallowed until the page was reloaded.
                    // Keep the document-level "click outside" listener (below)
                    // from closing the menu this click is about to open: the
                    // press is swallowed at the toggle. Without it, every
                    // mousedown on a toggle closed the open menu and the
                    // following click reopened the same one, so a second click
                    // on File could never close it.
                    let swallow = Closure::<dyn FnMut(MouseEvent)>::new(|evt: MouseEvent| {
                        evt.stop_propagation();
                    });
                    toggle
                        .add_event_listener_with_callback("mousedown", swallow.as_ref().unchecked_ref())
                        .ok();
                    swallow.forget();

                    let dd = dropdown.clone();
                    let tg = toggle.clone();
                    let menu = open_handle.clone();
                    let toggle_closure =
                        Closure::<dyn FnMut(MouseEvent)>::new(move |evt: MouseEvent| {
                            // Second click on the same toggle closes it; any
                            // other toggle closes what is open and opens this.
                            if menu_open_is(&menu, &dd) {
                                close_open_menu(&menu);
                            } else {
                                close_open_menu(&menu);
                                open_menu_entry(&menu, &dd, Some(tg.as_ref()));
                            }
                            evt.stop_propagation();
                        });
                    toggle
                        .add_event_listener_with_callback(
                            "click",
                            toggle_closure.as_ref().unchecked_ref(),
                        )
                        .ok();
                    toggle_closure.forget();

                    wrapper.append_child(toggle.as_ref()).ok();
                    wrapper.append_child(&dropdown).ok();
                    bar.append_child(&wrapper).ok();
                    let toggle_elem: Element =
                        <HtmlButtonElement as AsRef<Element>>::as_ref(&toggle).clone();
                    entries.push(MenuEntry {
                        label: label.clone(),
                        toggle: toggle_elem,
                        dropdown: dropdown.clone(),
                        items: entry_items,
                    });
                }
            }
        }

        // The two other ways a menu closes, both DOM-level because a menu has
        // no modal loop of its own:
        //
        //   * a click anywhere outside it (the page, the sheet canvas, another
        //     window's chrome) -- the `click outside` rule every menubar has;
        //   * `Escape`, wherever focus is.
        //
        // Both go through `close_open_menu`, so the bar's `open` cell (which
        // decides whether keys are routed as mnemonics) is cleared with the
        // DOM, and typing works again after a menu is dismissed.
        {
            let menu = open_handle.clone();
            let dismiss = Closure::<dyn FnMut(Event)>::new(move |evt: Event| {
                if !menu_active_in(&menu) {
                    return;
                }
                // A click inside the open menu, or on the bar, is handled by
                // the handlers above (they stop propagation); anything that
                // reaches the document is outside.
                close_open_menu(&menu);
                // Keep the click that dismissed the menu from also acting on
                // whatever it landed on: the first click after a menu should
                // close it, not press a cell.
                evt.stop_propagation();
                evt.prevent_default();
            });
            if let Some(win) = web_sys::window() {
                if let Some(doc) = win.document() {
                    let _ = doc.add_event_listener_with_callback(
                        "mousedown",
                        dismiss.as_ref().unchecked_ref(),
                    );
                }
            }
            // Leaked like the canvas listeners: the menubar lives for the
            // page's lifetime.
            dismiss.forget();
        }

        Ok(MenuBar {
            elem: bar,
            entries: Rc::new(RefCell::new(entries)),
            open: open_handle,
            selected: Rc::new(RefCell::new(0)),
            closures: Rc::new(RefCell::new(Vec::new())),
        })
    }

#[derive(Clone)]
pub struct SimpleAction {
    name: String,
}

impl SimpleAction {
        pub fn connect_activate<F: FnMut(*mut c_void) + 'static>(
            &self,
            f: F,
        ) -> Result<u64, Error> {
            register_action(&self.name, Box::new(f));
            Ok(0)
        }
    }

    pub fn create_simple_action(name: &str) -> Result<SimpleAction, Error> {
        Ok(SimpleAction {
            name: name.to_owned(),
        })
    }

    // -----------------------------------------------------------------------
    // Dialog
    // -----------------------------------------------------------------------
    #[derive(Clone)]
    pub struct Dialog {
        elem: HtmlDialogElement,
        content_area: HtmlDivElement,
        response_cb: Rc<RefCell<Option<Box<dyn FnMut(i32)>>>>,
        /// Whether `set_transient_for` asked for a *modal* presentation.
        /// See [`Dialog::set_transient_for`].
        modal: Rc<Cell<bool>>,
    }

    impl AsElement for Dialog {
        fn as_element(&self) -> &Element {
            self.elem.as_ref()
        }
    }

    impl Widget for Dialog {
        fn raw_handle(&self) -> *mut c_void {
            &self.elem as *const HtmlDialogElement as *mut c_void
        }
    }

    impl AsRef<*mut c_void> for Dialog {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.raw_handle() as *const *mut c_void) }
        }
    }

    impl Dialog {
        pub fn set_title(&self, title: &str) {
            let h2 = create_element("h2");
            h2.set_text_content(Some(title));
            self.elem.insert_before(&h2, self.elem.first_child().as_ref())
                .ok();
        }

        pub fn set_default_size(&self, w: i32, h: i32) {
            if w > 0 {
                set_css(self.elem.as_ref(), "width", &format!("{}px", w));
            }
            if h > 0 {
                set_css(self.elem.as_ref(), "height", &format!("{}px", h));
            }
        }

        pub fn add_button(&self, text: &str, response_id: i32) {
            let btn: HtmlButtonElement = create_element("button").dyn_into().unwrap();
            btn.set_text_content(Some(text));
            let cb = self.response_cb.clone();
            let elem = self.elem.clone();
            let closure = Closure::<dyn FnMut(MouseEvent)>::new(move |_: MouseEvent| {
                if let Some(ref mut f) = *cb.borrow_mut() {
                    f(response_id);
                }
                // Dismiss like every other backend: confirming must close.
                elem.close();
            });
            btn.add_event_listener_with_callback("click", closure.as_ref().unchecked_ref())
                .ok();
            closure.forget();
            self.elem.append_child(btn.as_ref()).ok();
        }

        /// Mark the dialog as modal, which is what `set_transient_for` means
        /// on a browser.
        ///
        /// GTK parents the dialog to a window so the WM centres it there; a
        /// page has no window manager, and the DOM's equivalent of "belongs
        /// to this window" is `showModal` -- which blocks interaction with the
        /// rest of the page, exactly as a parent-modal dialog does. The parent
        /// handle itself is not meaningful, so it is accepted and ignored.
        pub fn set_transient_for(&self, _parent: *mut c_void) {
            // A `present` that ran before `showModal` would defeat the
            // purpose, so this is a flag rather than a call: the actual
            // `show_modal` happens in `present`, which is the only place the
            // element is shown.
            self.modal.set(true);
        }

        /// Whether a `set_transient_for` asked for modality.
        fn wants_modal(&self) -> bool {
            self.modal.get()
        }

        pub fn get_content_area(&self) -> *mut c_void {
            &self.content_area as *const HtmlDivElement as *mut c_void
        }

        pub fn append_content_area(&self, child: &impl AsRef<*mut c_void>) {
            self.content_area.append_child(as_element_from_ptr(*child.as_ref())).ok();
        }

        /// Show the dialog, modally if `set_transient_for` asked for it.
        ///
        /// `show_modal` additionally blocks the rest of the page, which is
        /// the browser's version of a parent-modal dialog, and fires
        /// `preventDefault` on a cancel event so Escape cannot dismiss it
        /// behind the app's back -- the app decides what closing means.
        pub fn present(&self) {
            if self.wants_modal() {
                let _ = self.elem.show_modal();
            } else {
                let _ = self.elem.show();
            }
        }

        pub fn connect_response<F: FnMut(i32) + 'static>(&self, f: F) -> Result<u64, Error> {
            *self.response_cb.borrow_mut() = Some(Box::new(f));
            Ok(0)
        }

        pub fn close(&self) {
            self.elem.close();
        }

        /// Show or hide the dialog. NWG's `set_visible` on a dialog is
        /// `ShowWindow(SW_SHOW/HIDE)`; the DOM equivalent of both is the
        /// `<dialog>` element's own `open` attribute, so this is
        /// `show()`/`close()` rather than a CSS `display` toggle (which would
        /// leave a closed `<dialog>` still participating in `showModal`'s
        /// modality bookkeeping).
        pub fn set_visible(&self, visible: bool) {
            if visible {
                self.present();
            } else {
                self.elem.close();
            }
        }

        /// Settle the dialog's layout before it is shown.
        ///
        /// GTK and NWG both have to do real work here: they position the
        /// content and the button row in window coordinates, and a caller
        /// attaching a custom view needs the size settled before the window
        /// opens. A `<dialog>`'s layout is done by the browser's own box model
        /// and reflows on every frame, so there is nothing to compute and
        /// nothing to cache -- this exists so `common::Dialog::layout_dialog`
        /// resolves and the shared GUI's call is a documented no-op rather
        /// than a missing method.
        pub fn layout_dialog(&self) {}

        /// Present the dialog and return the response id.
        ///
        /// This is the one place a browser genuinely cannot supply what the
        /// signature wants, and the fallback is stated rather than faked.
        /// `gtk_dialog_run` blocks in a *nested* main loop until the user
        /// answers, so the caller gets a response id synchronously. A page has
        /// one event loop and cannot nest: `present()` returns immediately and
        /// the answer, if any, arrives later through `connect_response`.
        ///
        /// So the honest answer is the same one `backends_nwg_adapter.rs` gives
        /// (`pub fn run(&self) -> i32 { 0 }`) — present, and report "no response
        /// id". 0 is not a valid GTK response id (those are 1, 2, ...), so a
        /// caller comparing against it gets an unambiguous "none", and the
        /// `connect_response` path remains the one that carries the real id.
        pub fn run(&self) -> i32 {
            self.present();
            0
        }

        /// Mark a button as the one a dismiss acts on, GTK's
        /// `set_default_response`.
        ///
        /// The DOM has `autofocus`, which is the same idea: it is the control
        /// the browser focuses when the dialog appears. The response id
        /// itself is a GTK concept (it identifies the button in the response
        /// callback) and a `<dialog>` has no such id, so only the focus
        /// behaviour is expressed -- which is the part a user can see.
        pub fn set_default_response(&self, response_id: i32) {
            // Count down from the last-added button: GTK's default is
            // conventionally the affirmative action, and `add_button` appends
            // in call order, so the last one is the most likely candidate.
            // A caller that wants a different button passes its own id and
            // the mapping below lands on whichever button was added with it.
            let buttons = self.elem.get_elements_by_tag_name("button");
            let idx = (response_id - 1).clamp(0, buttons.length() as i32 - 1) as u32;
            if let Some(btn) = buttons.item(idx) {
                let _ = js_sys::Reflect::set(btn.as_ref(), &wasm_bindgen::JsValue::from_str("autofocus"), &wasm_bindgen::JsValue::TRUE);
            }
        }
    }

    pub fn create_dialog() -> Result<Dialog, Error> {
        let elem: HtmlDialogElement = create_element("dialog").dyn_into().map_err(|e| {
            Error::Backend(format!("create_dialog: {:?}", e))
        })?;
        let content: HtmlDivElement = create_element("div").dyn_into().map_err(|e| {
            Error::Backend(format!("create_dialog content: {:?}", e))
        })?;
        elem.append_child(content.as_ref()).ok();
        body().append_child(elem.as_ref()).ok();
        Ok(Dialog {
            elem,
            content_area: content,
            response_cb: Rc::new(RefCell::new(None)),
            modal: Rc::new(Cell::new(false)),
        })
    }

    // -----------------------------------------------------------------------
    // DropDown
    // -----------------------------------------------------------------------
    pub struct DropDown {
        elem: HtmlSelectElement,
        closures: Rc<RefCell<Vec<Box<dyn Any>>>>,
        next_id: Rc<RefCell<u64>>,
    }

    impl AsElement for DropDown {
        fn as_element(&self) -> &Element {
            self.elem.as_ref()
        }
    }

    /// The `&*mut c_void` escape hatch `common_types_mod!` requires for
    /// every widget, used where the shared layer takes a raw handle.
    ///
    /// Routed through [`Widget::raw_handle`] like every other `AsRef` in this
    /// module, rather than casting the field directly: the `Widget` impl is
    /// the single place that says what this widget's handle is, so a change
    /// to it cannot silently desynchronise the two.
    ///
    /// This and the three below are not in §6 of `WASM_MISSING.md` — the
    /// static diff reported them as present (they were, on the pre-merge
    /// working tree) while the committed adapter had dropped them, so the
    /// merge surfaced them as `E0277: the trait bound `DropDown:
    /// AsRef<*mut c_void>` is not satisfied`. A widget missing `AsRef` fails
    /// the build, so this is a real gap rather than a cosmetic one.
    impl AsRef<*mut c_void> for DropDown {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.raw_handle() as *const *mut c_void) }
        }
    }

    impl Widget for DropDown {
        fn raw_handle(&self) -> *mut c_void {
            &self.elem as *const HtmlSelectElement as *mut c_void
        }
    }

    impl Clone for DropDown {
        fn clone(&self) -> Self {
            DropDown {
                elem: self.elem.clone(),
                closures: self.closures.clone(),
                next_id: self.next_id.clone(),
            }
        }
    }

    impl DropDown {
        /// Select `index`, or clear the selection with `None`.
        ///
        /// The `Option` is GTK's contract (a caller can clear the selection),
        /// and the DOM has a matching state: `selectedIndex = -1` is "nothing
        /// selected", which is exactly what a `None` asks for. Passing a bare
        /// `u32` -- the old signature -- could not express it, so a caller that
        /// wanted "no selection" had no way to say so.
        pub fn set_active(&self, index: Option<u32>) {
            match index {
                Some(i) => self.elem.set_selected_index(i as i32),
                None => self.elem.set_selected_index(-1),
            }
        }

        /// The selected index, or `None` when nothing is selected. `None` is
        /// what `selectedIndex == -1` means, and it is the inverse of
        /// `set_active(None)`.
        pub fn get_active(&self) -> i32 {
            self.elem.selected_index()
        }

        /// The selected index as an `Option`, for callers that would rather
        /// not interpret the `-1` sentinel themselves.
        pub fn active(&self) -> Option<u32> {
            match self.elem.selected_index() {
                i if i < 0 => None,
                i => Some(i as u32),
            }
        }

        pub fn grab_focus(&self) {
            let _ = self.elem.focus();
        }

        pub fn set_visible(&self, v: bool) {
            set_css(self.elem.as_ref(), "display", if v { "" } else { "none" });
        }

        pub fn set_size_request(&self, w: i32, h: i32) {
            if w > 0 {
                set_css(self.elem.as_ref(), "width", &format!("{}px", w));
            }
            if h > 0 {
                set_css(self.elem.as_ref(), "height", &format!("{}px", h));
            }
        }

        pub fn set_hexpand(&self, expand: bool) {
            if expand {
                set_css(self.elem.as_ref(), "flex-grow", "1");
                set_css(self.elem.as_ref(), "align-self", "stretch");
            } else {
                set_css(self.elem.as_ref(), "flex-grow", "0");
            }
        }

        pub fn set_vexpand(&self, expand: bool) {
            if expand {
                set_css(self.elem.as_ref(), "flex-shrink", "1");
                set_css(self.elem.as_ref(), "align-self", "stretch");
            } else {
                set_css(self.elem.as_ref(), "flex-shrink", "0");
            }
        }

        /// Move the dropdown within its parent, in px.
        ///
        /// The DOM answer to GTK's `set_offset`, and what makes a dropdown
        /// hosted in an `Overlay` land where the caller asked instead of at
        /// the overlay's top-left corner: `add_overlay` sets
        /// `position: absolute` with no offset, so without this a positioned
        /// popup has nowhere to go. `transform: translate` is used rather than
        /// `left`/`top` because it composes with any layout position the
        /// overlay already established.
        pub fn set_offset(&self, x: i32, y: i32) {
            set_css(
                self.elem.as_ref(),
                "transform",
                &format!("translate({}px, {}px)", x, y),
            );
        }

        pub fn connect_changed(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            let cb = Rc::new(RefCell::new(f));
            let cb2 = cb.clone();
            let closure = Closure::<dyn FnMut(Event)>::new(move |_: Event| {
                (cb2.borrow_mut())();
            });
            self.elem
                .add_event_listener_with_callback("change", closure.as_ref().unchecked_ref())
                .map_err(|e| Error::Backend(format!("connect_changed: {:?}", e)))?;
            let id = *self.next_id.borrow();
            *self.next_id.borrow_mut() += 1;
            self.closures.borrow_mut().push(Box::new(closure));
            Ok(id)
        }
    }

    pub fn create_dropdown(items: &[&str]) -> Result<DropDown, Error> {
        let elem: HtmlSelectElement = create_element("select").dyn_into().map_err(|e| {
            Error::Backend(format!("create_dropdown: {:?}", e))
        })?;
        for item in items {
            let opt: HtmlOptionElement = create_element("option").dyn_into().unwrap();
            opt.set_text_content(Some(item));
            elem.append_child(opt.as_ref()).ok();
        }
        Ok(DropDown {
            elem,
            closures: Rc::new(RefCell::new(Vec::new())),
            next_id: Rc::new(RefCell::new(1)),
        })
    }

    // -----------------------------------------------------------------------
    // CheckButton
    // -----------------------------------------------------------------------
    pub struct CheckButton {
        elem: Element,
        input: HtmlInputElement,
        closures: Rc<RefCell<Vec<Box<dyn Any>>>>,
        next_id: Rc<RefCell<u64>>,
    }

    impl AsElement for CheckButton {
        fn as_element(&self) -> &Element {
            &self.elem
        }
    }

    /// See [`DropDown`]'s `AsRef` for why these four exist.
    ///
    /// This one hands back the `<input>`, not the wrapper `elem`, because the
    /// input is the element the shared layer wants to reparent when it packs
    /// a check button into a box.
    impl AsRef<*mut c_void> for CheckButton {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.input as *const HtmlInputElement as *const *mut c_void) }
        }
    }

    impl Widget for CheckButton {
        fn raw_handle(&self) -> *mut c_void {
            &self.elem as *const Element as *mut c_void
        }
    }

    impl Clone for CheckButton {
        fn clone(&self) -> Self {
            CheckButton {
                elem: self.elem.clone(),
                input: self.input.clone(),
                closures: self.closures.clone(),
                next_id: self.next_id.clone(),
            }
        }
    }

    impl CheckButton {
        pub fn is_active(&self) -> bool {
            self.input.checked()
        }

        pub fn set_active(&self, active: bool) {
            self.input.set_checked(active);
        }

        pub fn connect_toggled(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            let cb = Rc::new(RefCell::new(f));
            let cb2 = cb.clone();
            let closure = Closure::<dyn FnMut(Event)>::new(move |_: Event| {
                (cb2.borrow_mut())();
            });
            self.input
                .add_event_listener_with_callback("change", closure.as_ref().unchecked_ref())
                .map_err(|e| Error::Backend(format!("connect_toggled: {:?}", e)))?;
            let id = *self.next_id.borrow();
            *self.next_id.borrow_mut() += 1;
            self.closures.borrow_mut().push(Box::new(closure));
            Ok(id)
        }

        pub fn set_visible(&self, v: bool) {
            set_css(&self.elem, "display", if v { "" } else { "none" });
        }
        pub fn set_hexpand(&self, expand: bool) {
            if expand {
                set_css(&self.elem, "flex-grow", "1");
                set_css(&self.elem, "align-self", "stretch");
            } else {
                set_css(&self.elem, "flex-grow", "0");
            }
        }
        pub fn set_vexpand(&self, expand: bool) {
            if expand {
                set_css(&self.elem, "flex-shrink", "1");
                set_css(&self.elem, "align-self", "stretch");
            } else {
                set_css(&self.elem, "flex-shrink", "0");
            }
        }
        pub fn set_size_request(&self, w: i32, h: i32) {
            if w > 0 {
                set_css(&self.elem, "width", &format!("{}px", w));
            }
            if h > 0 {
                set_css(&self.elem, "height", &format!("{}px", h));
            }
        }

        /// Replace the label text.
        ///
        /// The wrapper is a `<label>` holding the input plus a text node, so
        /// the text is the wrapper's *last* child; only that one is rewritten
        /// and the input is left alone.
        pub fn set_label(&self, label: &str) {
            if let Some(node) = self.elem.last_child() {
                let _ = node.set_text_content(Some(label));
            }
        }

        /// The current label text, read back from the same text node
        /// [`CheckButton::set_label`] writes.
        pub fn get_label(&self) -> Option<String> {
            self.elem.last_child().and_then(|n| n.text_content())
        }
    }

    pub fn create_checkbutton(label: &str) -> Result<CheckButton, Error> {
        let wrapper = create_element("label");
        let input: HtmlInputElement = create_element("input").dyn_into().unwrap();
        input.set_type("checkbox");
        wrapper.append_child(input.as_ref()).ok();
        wrapper
            .append_child(&web_sys::Text::new_with_data(label).unwrap())
            .ok();
        Ok(CheckButton {
            elem: wrapper,
            input,
            closures: Rc::new(RefCell::new(Vec::new())),
            next_id: Rc::new(RefCell::new(1)),
        })
    }

    // -----------------------------------------------------------------------
    // RadioButton
    // -----------------------------------------------------------------------
    pub struct RadioButton {
        elem: Element,
        input: HtmlInputElement,
        closures: Rc<RefCell<Vec<Box<dyn Any>>>>,
        next_id: Rc<RefCell<u64>>,
        group_name: String,
    }

    impl AsElement for RadioButton {
        fn as_element(&self) -> &Element {
            &self.elem
        }
    }

    /// See [`DropDown`]'s `AsRef` for why these four exist.
    impl AsRef<*mut c_void> for RadioButton {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.input as *const HtmlInputElement as *const *mut c_void) }
        }
    }

    impl Widget for RadioButton {
        fn raw_handle(&self) -> *mut c_void {
            &self.elem as *const Element as *mut c_void
        }
    }

    impl Clone for RadioButton {
        fn clone(&self) -> Self {
            RadioButton {
                elem: self.elem.clone(),
                input: self.input.clone(),
                closures: self.closures.clone(),
                next_id: self.next_id.clone(),
                group_name: self.group_name.clone(),
            }
        }
    }

    impl RadioButton {
        pub fn is_active(&self) -> bool {
            self.input.checked()
        }

        pub fn set_active(&self, active: bool) {
            self.input.set_checked(active);
        }

        pub fn connect_toggled(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            let cb = Rc::new(RefCell::new(f));
            let cb2 = cb.clone();
            let closure = Closure::<dyn FnMut(Event)>::new(move |_: Event| {
                (cb2.borrow_mut())();
            });
            self.input
                .add_event_listener_with_callback("change", closure.as_ref().unchecked_ref())
                .map_err(|e| Error::Backend(format!("connect_toggled: {:?}", e)))?;
            let id = *self.next_id.borrow();
            *self.next_id.borrow_mut() += 1;
            self.closures.borrow_mut().push(Box::new(closure));
            Ok(id)
        }

        pub fn set_visible(&self, v: bool) {
            set_css(&self.elem, "display", if v { "" } else { "none" });
        }
        pub fn set_hexpand(&self, expand: bool) {
            if expand {
                set_css(&self.elem, "flex-grow", "1");
                set_css(&self.elem, "align-self", "stretch");
            } else {
                set_css(&self.elem, "flex-grow", "0");
            }
        }
        pub fn set_vexpand(&self, expand: bool) {
            if expand {
                set_css(&self.elem, "flex-shrink", "1");
                set_css(&self.elem, "align-self", "stretch");
            } else {
                set_css(&self.elem, "flex-shrink", "0");
            }
        }
        pub fn set_size_request(&self, w: i32, h: i32) {
            if w > 0 {
                set_css(&self.elem, "width", &format!("{}px", w));
            }
            if h > 0 {
                set_css(&self.elem, "height", &format!("{}px", h));
            }
        }
        pub fn grab_focus(&self) {
            let _ = self.input.focus();
        }
        /// See [`CheckButton::set_label`].
        pub fn set_label(&self, label: &str) {
            if let Some(node) = self.elem.last_child() {
                let _ = node.set_text_content(Some(label));
            }
        }
        /// See [`CheckButton::get_label`].
        pub fn get_label(&self) -> Option<String> {
            self.elem.last_child().and_then(|n| n.text_content())
        }
    }

    pub fn create_radiobutton(group: Option<&RadioButton>, label: &str) -> Result<RadioButton, Error> {
        let group_name = group
            .map(|g| g.group_name.clone())
            .unwrap_or_else(|| format!("rb_{}", rand_id()));
        let wrapper = create_element("label");
        let input: HtmlInputElement = create_element("input").dyn_into().unwrap();
        input.set_type("radio");
        input.set_name(&group_name);
        wrapper.append_child(input.as_ref()).ok();
        wrapper
            .append_child(&web_sys::Text::new_with_data(label).unwrap())
            .ok();
        Ok(RadioButton {
            elem: wrapper,
            input,
            closures: Rc::new(RefCell::new(Vec::new())),
            next_id: Rc::new(RefCell::new(1)),
            group_name,
        })
    }

    fn rand_id() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    // -----------------------------------------------------------------------
    // TextView
    // -----------------------------------------------------------------------
    pub struct TextView {
        elem: HtmlTextAreaElement,
        closures: Rc<RefCell<Vec<Box<dyn Any>>>>,
        next_id: Rc<RefCell<u64>>,
    }

    impl AsElement for TextView {
        fn as_element(&self) -> &Element {
            self.elem.as_ref()
        }
    }

    /// See [`DropDown`]'s `AsRef` for why these four exist.
    impl AsRef<*mut c_void> for TextView {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.raw_handle() as *const *mut c_void) }
        }
    }

    impl Widget for TextView {
        fn raw_handle(&self) -> *mut c_void {
            &self.elem as *const HtmlTextAreaElement as *mut c_void
        }
    }

    impl Clone for TextView {
        fn clone(&self) -> Self {
            TextView {
                elem: self.elem.clone(),
                closures: self.closures.clone(),
                next_id: self.next_id.clone(),
            }
        }
    }

    impl TextView {
        pub fn set_text(&self, text: &str) {
            self.elem.set_value(text);
        }

        pub fn get_text(&self) -> Option<String> {
            Some(self.elem.value())
        }

        pub fn set_wrap_mode(&self, wrap_mode: i32) {
            match wrap_mode {
                0 => self.elem.set_wrap("off"),
                1 => self.elem.set_wrap("soft"),
                2 => self.elem.set_wrap("hard"),
                _ => self.elem.set_wrap("soft"),
            }
        }

        pub fn set_size_request(&self, w: i32, h: i32) {
            if w > 0 {
                self.elem.set_cols(w as u32);
            }
            if h > 0 {
                self.elem.set_rows(h as u32);
            }
        }

        pub fn set_hexpand(&self, expand: bool) {
            if expand {
                set_css(self.elem.as_ref(), "flex-grow", "1");
            } else {
                set_css(self.elem.as_ref(), "flex-grow", "0");
            }
        }

        pub fn set_vexpand(&self, expand: bool) {
            if expand {
                set_css(self.elem.as_ref(), "flex-shrink", "1");
                set_css(self.elem.as_ref(), "align-self", "stretch");
            } else {
                set_css(self.elem.as_ref(), "flex-shrink", "0");
            }
        }

        pub fn set_visible(&self, v: bool) {
            set_css(self.elem.as_ref(), "display", if v { "" } else { "none" });
        }

        /// Read-only mode, via the `readOnly` attribute NWG's `set_editable`
        /// sets. `readOnly` rather than `disabled` on purpose: a disabled
        /// textarea drops out of the tab order and dims, whereas a read-only
        /// one stays selectable and copyable, which is what "not editable"
        /// means in a form.
        pub fn set_editable(&self, editable: bool) {
            self.elem.set_read_only(!editable);
        }
            /// Append a line to the buffer.
        ///
        /// Read-modify-write: this backend has no incremental insert, so the text is
        /// fetched, extended and written back. Correct, but O(document) per call.
        /// A high-rate log pane should reimplement this against the native handle.
        pub fn append_text(&self, text: &str) {
        let mut buf = self.get_text().unwrap_or_default();
        buf.push_str(text);
        self.set_text(&buf);
        }
    }

    pub fn create_textview() -> Result<TextView, Error> {
        let elem: HtmlTextAreaElement = create_element("textarea").dyn_into().map_err(|e| {
            Error::Backend(format!("create_textview: {:?}", e))
        })?;
        Ok(TextView {
            elem,
            closures: Rc::new(RefCell::new(Vec::new())),
            next_id: Rc::new(RefCell::new(1)),
        })
    }

    // -----------------------------------------------------------------------
    // DrawContext (Canvas 2D)
    // -----------------------------------------------------------------------
    pub struct WasmDrawContext {
        ctx: web_sys::CanvasRenderingContext2d,
        /// The context state already set, so a repeated value is not written
        /// again.
        ///
        /// A canvas keeps `font` and `fillStyle` between calls, and assigning
        /// either parses a CSS value -- 6662 fill-style writes and 380 font
        /// writes per frame for a handful of distinct values, every one of them
        /// across the wasm/JS boundary. Skipping a write that would not change
        /// anything is measured: it takes the per-cell cost out of the frame.
        state: RefCell<FrameState>,
    }

    /// See [`WasmDrawContext::state`].
    #[derive(Default)]
    struct FrameState {
        font: Option<String>,
        fill: Option<String>,
    }

    impl WasmDrawContext {
        fn set_font(&self, font: &str, size: f64, slant: i32, weight: i32) {
            let weight_str = if weight != 0 { "bold" } else { "" };
            let slant_str = if slant != 0 { "italic " } else { "" };
            let size_str = format!("{}px", size);
            let f = format!("{}{} {} {}", slant_str, weight_str, size_str, font);
            let mut state = self.state.borrow_mut();
            if state.font.as_ref() != Some(&f) {
                let _ = self.ctx.set_font(&f);
                state.font = Some(f);
            }
        }

        /// Set `fillStyle`, unless the context already has that colour.
        fn set_fill(&self, r: f64, g: f64, b: f64, a: f64) {
            let colour = Self::rgba(r, g, b, a);
            let mut state = self.state.borrow_mut();
            if state.fill.as_ref() != Some(&colour) {
                let _ = self.ctx.set_fill_style_str(&colour);
                state.fill = Some(colour);
            }
        }

        fn rgba(r: f64, g: f64, b: f64, a: f64) -> String {
            format!("rgba({},{},{},{})", (r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8, a)
        }
    }

    impl crate::core::DrawContext for WasmDrawContext {
        fn fill_rect(&mut self, x: f64, y: f64, w: f64, h: f64, r: f64, g: f64, b: f64, a: f64) {
            // A fill with no area is a no-op everywhere else (Cairo and GDI
            // both ignore it), and here it *throws*: `fillRect` rejects a
            // non-finite argument, and one call site passes the width of an
            // empty string's underline. Ignoring it keeps a frame painting
            // instead of aborting at the first empty cell.
            if !(w > 0.0 && h > 0.0) {
                return;
            }
            self.set_fill(r, g, b, a);
            self.ctx.fill_rect(x, y, w, h);
        }

        fn stroke_rect(&mut self, x: f64, y: f64, w: f64, h: f64, r: f64, g: f64, b: f64, a: f64, lw: f64) {
            // A stroke straddles its path, so a 1px line at an integer
            // coordinate is half outside the shape it outlines and half
            // outside the cell -- and under a scale it is antialiased across
            // two device pixels at 50%. Inset by half the line width: the path
            // then falls on the pixel boundary the way GDI/Cairo's
            // `stroke_rect` at these coordinates is drawn.
            if !(w > 0.0 && h > 0.0) {
                return;
            }
            // Sub-pixel widths are drawn as a filled edge instead: a 0.5px
            // stroke is what the shared renderer asks for on a cell boundary
            // (`stroke_rect(.., 0.5)`), and a canvas hairline is antialiased
            // across two device pixels at half the requested opacity -- faint
            // enough that a grid whose cells are all the same fill looks like
            // one blank pane. A 1px fill at the same alpha is the same line,
            // drawn at the intensity that was asked for.
            //
            // Only the horizontal edge is drawn for a wide, shallow box, which
            // is what every 0.5px call in this codebase is: a cell's bottom
            // rule, the underline of a hyperlink, the top edge of a band.
            // Drawing all four sides of a cell would put a hairline down every
            // column boundary at twice the intensity the caller asked for, and
            // on a grid whose columns are all the same fill that reads as a
            // change to the layout rather than to the line.
            if lw < 1.0 {
                let _ = self.ctx.set_fill_style_str(&Self::rgba(r, g, b, a));
                if w >= h {
                    self.ctx.fill_rect(x.floor(), y.floor(), w.round().max(1.0), 1.0);
                } else {
                    self.ctx.fill_rect(x.floor(), y.floor(), 1.0, h.round().max(1.0));
                }
                // The snap below is for the *filled* edge; the 0.5px cell rule
                // that callers also draw is deliberately left as a stroke. A
                // filled hairline reads as a hard gridline and changes the
                // layout the eyes see; a stroked one is the faint rule the
                // shared renderer means.
                return;
            }
            let inset = lw / 2.0;
            let _ = self.ctx.set_stroke_style_str(&Self::rgba(r, g, b, a));
            self.ctx.set_line_width(lw);
            self.ctx.stroke_rect(x + inset, y + inset, w - lw, h - lw);
        }

        fn draw_text_styled(&mut self, x: f64, y: f64, text: &str, font: &str, size: f64,
                            r: f64, g: f64, b: f64, a: f64, slant: i32, weight: i32) {
            self.set_font(font, size, slant, weight);
            self.set_fill(r, g, b, a);
            // `y` is the text *top*, as on GTK, and there are two ways to
            // honour that in a DOM canvas: add the ascent to get the baseline,
            // or set `textBaseline = "top"` and pass `y` through. The second is
            // what this does, because `top` is defined as the top of the *em
            // box*, i.e. exactly the origin the shared renderer positions
            // against -- no metric has to be re-derived, and the glyphs land
            // inside the caller's box rather than above it. (An earlier version
            // added an ascent to `y` and then nudged the result; the nudge was
            // a fudge factor for the mismatch it had introduced, and it is gone
            // with the ascent.)
            let _ = self.ctx.set_text_baseline("top");
            let _ = self.ctx.fill_text(text, x, y);
        }

        fn text_extents_styled(&self, text: &str, font: &str, size: f64, slant: i32, weight: i32) -> (f64, f64, f64, f64) {
            // Set the font directly rather than save/restore around it: a
            // measure must not disturb the caller's font *within a frame*,
            // but the next draw sets the font again for its own text (and
            // `set_font` skips a redundant write), so the save/restore pair --
            // two more cross-boundary calls per measured string, and 62 per
            // frame here -- buys nothing.
            self.set_font(font, size, slant, weight);
            let Some(metrics) = self.ctx.measure_text(text).ok() else {
                return (0.0, 0.0, 0.0, 0.0);
            };
            let w = metrics.width();
            let xb = -metrics.actual_bounding_box_left();
            let yb = -metrics.actual_bounding_box_ascent();
            let h = metrics.actual_bounding_box_ascent() + metrics.actual_bounding_box_descent();
            (xb, yb, w, h)
        }

        /// Paint the whole surface.
        ///
        /// This has to ignore the current path, clip and transform, where
        /// `fill_rect` honours all three: the caller's clip is not reset
        /// between frames (nothing un-`clip`s in this adapter), so a `clear`
        /// that filled a rect would be clipped to the *previous* frame's
        /// clip -- and a first frame clipped to a stale region leaves the rest
        /// of the canvas as the browser cleared it. `fillRect` under an
        /// identity transform over the backing store's own pixel size is the
        /// one operation that always covers everything.
        fn clear(&mut self, r: f64, g: f64, b: f64, a: f64) {
            let _ = self.ctx.save();
            let _ = self.ctx.set_transform(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);
            let _ = self.ctx.set_fill_style_str(&Self::rgba(r, g, b, a));
            let canvas = self.ctx.canvas().unwrap();
            let w = canvas.width() as f64;
            let h = canvas.height() as f64;
            self.ctx.fill_rect(0.0, 0.0, w, h);
            let _ = self.ctx.restore();
        }

        /// Blit a straight-alpha RGBA8 image at `(x, y)`, scaled by `scale`.
        ///
        /// The only `DrawContext` method GTK overrides that this backend did
        /// not, so every caller fell through to the trait default and got
        /// `false` -- which is the "I could not draw this, degrade to vector
        /// primitives" answer, and is why an image preview on a page showed
        /// nothing at all.
        ///
        /// Two steps, because there is no `putImageDataAt`-with-transform:
        /// `putImageData` writes at integer device pixels and ignores the
        /// transform, and `putImageData` is the only way to get *premultiplied*
        /// semantics right. The DOM wants **straight** alpha, which is what
        /// the caller supplies, so the pixels are staged verbatim into an
        /// `ImageData`; the scale and the sub-pixel origin are then applied by
        /// `drawImage` from that staging canvas, which does honour the
        /// transform.
        ///
        /// Scaling is done by `drawImage` with a destination rectangle rather
        /// than by resampling the `ImageData`, because the DOM's image
        /// smoothing is what a caller expects from a scaled blit (and matches
        /// what Cairo's own filter does on GTK).
        fn draw_rgba_image(
            &mut self,
            x: f64,
            y: f64,
            pixels: &[u8],
            width: u32,
            height: u32,
            scale: f64,
        ) -> bool {
            if width == 0 || height == 0 {
                // A zero-sized image has nothing to blit; `false` tells the
                // caller to fall back, which is the honest answer rather than
                // a silent no-op that looks like success.
                return false;
            }
            let expected = (width as usize) * (height as usize) * 4;
            if pixels.len() < expected {
                // Short buffer: the DOM would throw a DataCloneError on an
                // `ImageData` of the wrong length. Reporting `false` keeps the
                // caller's fallback path instead of panicking across the FFI
                // boundary.
                return false;
            }
            // `Clamped` is a marker wrapper the DOM's byte array type uses;
            // the slice is wrapped, not reinterpreted, so the bytes cross
            // unmodified. The argument order is (data, width, height).
            let Ok(image_data) = web_sys::ImageData::new_with_u8_clamped_array_and_sh(
                wasm_bindgen::Clamped(pixels),
                width,
                height,
            ) else {
                return false;
            };

            // A scratch canvas to hold the unscaled image, so the destination
            // rectangle can scale it. Created on demand and dropped after --
            // `drawImage` needs a source, and an `ImageData` is not one.
            let Ok(scratch) = create_element("canvas").dyn_into::<HtmlCanvasElement>() else {
                return false;
            };
            scratch.set_width(width);
            scratch.set_height(height);
            let Some(sctx) = scratch
                .get_context("2d")
                .ok()
                .flatten()
                .and_then(|c| c.dyn_into::<web_sys::CanvasRenderingContext2d>().ok())
            else {
                return false;
            };
            // Both calls below are fallible (they return `Result<_, JsValue>`
            // because a DOM exception is reportable across the boundary: a
            // detached canvas, or a scratch canvas with no 2d context). A
            // failure means the blit did not happen, so `false` is the
            // honest answer and lets the caller fall back to vector drawing
            // rather than silently drawing nothing.
            if sctx.put_image_data(&image_data, 0.0, 0.0).is_err() {
                return false;
            }

            let dw = width as f64 * scale;
            let dh = height as f64 * scale;
            // The `and_dw_and_dh` overload is the one that scales; the plain
            // form blits at the source's natural size and would ignore
            // `scale` entirely. There is no single 6-argument overload: each
            // source type has its own, so this names the canvas one.
            self.ctx
                .draw_image_with_html_canvas_element_and_dw_and_dh(
                    scratch.as_ref(),
                    x,
                    y,
                    dw,
                    dh,
                )
                .is_ok()
        }

        fn save(&mut self) { self.ctx.save(); }
        fn restore(&mut self) { self.ctx.restore(); }

        fn clip(&mut self, x: f64, y: f64, w: f64, h: f64) {
            self.ctx.begin_path();
            self.ctx.rect(x, y, w, h);
            self.ctx.clip();
        }
    }

    // -----------------------------------------------------------------------
    // Canvas
    // -----------------------------------------------------------------------
    pub struct Canvas {
        elem: HtmlCanvasElement,
        draw_cb: Rc<RefCell<Option<Box<dyn FnMut(&mut dyn crate::core::DrawContext, i32, i32)>>>>,
        click_cb: Rc<RefCell<Option<Box<dyn FnMut(f64, f64)>>>>,
        key_cb: Rc<RefCell<Option<Box<dyn FnMut(u32) -> bool>>>>,
        /// `(x, y, button, modifier mask)`, for a press.
        click_button_cb: Rc<RefCell<Option<Box<dyn FnMut(f64, f64, u32, u32)>>>>,
        /// `(x, y, state)`, for motion.
        motion_cb: Rc<RefCell<Option<Box<dyn FnMut(f64, f64, u32)>>>>,
        /// `(keyval, modifier mask) -> consumed`, for keys.
        key_raw_cb: Rc<RefCell<Option<Box<dyn FnMut(u32, u32) -> bool>>>>,
        /// The unified gesture stream, fed by pointer and wheel listeners.
        /// See [`Canvas::on_gesture`].
        gesture_cb: Rc<RefCell<Option<Box<dyn FnMut(crate::core::Gesture)>>>>,
        /// `(x, y, button, modifier mask)`, for a release.
        release_cb: Rc<RefCell<Option<Box<dyn FnMut(f64, f64, u32, u32)>>>>,
        /// The live CSS-box size, mirrored into the backing store by
        /// [`Canvas::sync_backing_store`]. See that method for why the two
        /// sizes are not the same number.
        ///
        /// `Cell` (not `RefCell`) on purpose: this is read and written from
        /// inside the draw path and from a `ResizeObserver` callback, and both
        /// can run while a `draw_cb` borrow is live. A `try_borrow` would have
        /// to degrade to "skip this frame", which is the blank-canvas bug
        /// again, one layer down.
        css_size: Rc<Cell<(f64, f64)>>,
        /// A frame is already scheduled; see [`Canvas::queue_redraw`].
        redraw_pending: Rc<Cell<bool>>,
        closures: Rc<RefCell<Vec<Box<dyn Any>>>>,
    }

    impl AsElement for Canvas {
        fn as_element(&self) -> &Element {
            self.elem.as_ref()
        }
    }

    impl Widget for Canvas {
        fn raw_handle(&self) -> *mut c_void {
            &self.elem as *const HtmlCanvasElement as *mut c_void
        }
    }

    impl AsRef<*mut c_void> for Canvas {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.raw_handle() as *const *mut c_void) }
        }
    }

    impl Clone for Canvas {
        fn clone(&self) -> Self {
            Canvas {
                elem: self.elem.clone(),
                draw_cb: self.draw_cb.clone(),
                click_cb: self.click_cb.clone(),
                key_cb: self.key_cb.clone(),
                click_button_cb: self.click_button_cb.clone(),
                motion_cb: self.motion_cb.clone(),
                key_raw_cb: self.key_raw_cb.clone(),
                gesture_cb: self.gesture_cb.clone(),
                release_cb: self.release_cb.clone(),
                css_size: self.css_size.clone(),
                redraw_pending: self.redraw_pending.clone(),
                closures: self.closures.clone(),
            }
        }
    }

    impl Canvas {
        pub fn set_draw_callback(&self, cb: Box<dyn FnMut(&mut dyn crate::core::DrawContext, i32, i32)>) {
            *self.draw_cb.borrow_mut() = Some(cb);
            // Register the size source *after* the callback: a ResizeObserver
            // reports the element's current box as soon as it is observed, so
            // a canvas that already has a size draws in this same turn.
            self.observe_css_size();
            self.queue_redraw();
        }

        /// Mirror the canvas's CSS box into its backing store, returning
        /// `(device_width, device_height)`.
        ///
        /// A `<canvas>` has two sizes and they are not the same number:
        /// `width`/`height` attributes are the backing store in *device*
        /// pixels, while CSS `width`/`height` are the element's box in *CSS*
        /// pixels. Draw code works in the backing store; pointer events report
        /// CSS pixels. Leaving the backing store at its default 300×150 while
        /// the box is stretched to a phone screen therefore draws one small
        /// image into the corner of a large, otherwise blank element.
        ///
        /// The CSS size is read from `getBoundingClientRect`, not from the
        /// `width`/`height` attributes, precisely so a stylesheet (flex
        /// growth, `height: 100%`) decides the size -- the same division of
        /// labour GTK has, where the allocation comes from the box and the
        /// widget asks for it.
        ///
        /// At least 1px in each direction: a zero-sized backing store makes
        /// `getContext('2d')` paint into nothing, and a display:none canvas
        /// reports exactly zero.
        fn sync_backing_store(&self) -> (i32, i32) {
            self.measure_with_backing_store().1
        }

        /// The box, and the backing-store size it implies.
        ///
        /// `.0` is the CSS size the *element* was laid out at; `.1` is the
        /// device size the *canvas* is. They differ by `devicePixelRatio` and
        /// by the fractional-pixel rounding, which is exactly why the caller
        /// that assigns `width`/`height` (and so resets the canvas) is the one
        /// that needs both.
        fn measure_with_backing_store(&self) -> ((f64, f64), (i32, i32)) {
            // Skip measuring a canvas that is not in the layout: a hidden
            // one reports 0x0, which would put the backing store at 1x1 and
            // hand the draw callback a surface that has to be redrawn from
            // scratch the moment the canvas is shown. `visibility`/`opacity`
            // are left out on purpose -- those keep the box, so the size is
            // real and the redraw is useful.
            let hidden = js_sys::Reflect::get(self.elem.as_ref(), &wasm_bindgen::JsValue::from_str("offsetParent"))
                .map(|v| v.is_null() || v.is_undefined())
                .unwrap_or(false)
                && self.elem.client_width() == 0
                && self.elem.client_height() == 0;
            if hidden {
                let w = self.elem.width().max(1) as i32;
                let h = self.elem.height().max(1) as i32;
                return (self.css_size.get(), (w, h));
            }
            let rect = self.elem.get_bounding_client_rect();
            let css_w = rect.width().max(0.0);
            let css_h = rect.height().max(0.0);
            self.css_size.set((css_w, css_h));
            // `devicePixelRatio` is the number of device pixels per CSS pixel;
            // on a 2x display a 400px-wide canvas needs an 800px backing store
            // or every glyph is upscaled and soft. A page zoom changes it, so
            // it is read per resize rather than cached.
            let dpr = web_sys::window()
                .map(|w| w.device_pixel_ratio())
                .unwrap_or(1.0)
                .clamp(1.0, 4.0);
            // Snap to whole device pixels, not up: a canvas is an inline
            // replaced element whose box is driven by the *attribute* size
            // when CSS leaves that dimension to the content, so a backing store
            // rounded up from a fractional box grows the box, which rounds the
            // next store up again -- a ladder that ends with the sheet taller
            // than the page. Rounding to nearest is a fixed point of that
            // relation: a box at 612.5px yields a 612px (or 613px) store, and
            // the box then stays where the layout put it.
            let dw = ((css_w * dpr).round() as u32).max(1) as i32;
            let dh = ((css_h * dpr).round() as u32).max(1) as i32;
            ((css_w, css_h), (dw, dh))
        }

        /// Assign the backing store, and draw if it moved.
        ///
        /// Assigning `width`/`height` *resets* the canvas: the bitmap is
        /// cleared and the 2d context goes back to its defaults (transform,
        /// clip, fill, font). A frame already on screen therefore must not be
        /// measured again unless it is about to be redrawn -- and a `queue_redraw`
        /// that resets the surface and then cannot draw (no context) would
        /// leave the canvas blank. So the reset and the draw are one step: the
        /// size is compared first, and only a real change re-enters the draw
        /// callback with the new surface.
        ///
        /// Returns the size to report to the caller: the *box* size while the
        /// backing store is unchanged (that is the size on screen), the new
        /// device size when the store was reset for it.
        fn sync_and_redraw(&self) -> Option<(i32, i32)> {
            let (css, device) = self.measure_with_backing_store();
            let unchanged = self.elem.width() as i32 == device.0 && self.elem.height() as i32 == device.1;
            if unchanged {
                return None;
            }
            self.elem.set_width(device.0 as u32);
            self.elem.set_height(device.1 as u32);
            let _ = css;
            Some(device)
        }

        /// Re-draw whenever the element's box changes, from the browser's own
        /// resize notifications.
        ///
        /// GTK gets this for free: the container allocates the drawing area
        /// and `set_draw_func` runs for every frame. A `ResizeObserver` is the
        /// DOM's equivalent -- it fires once when observation starts (with the
        /// current box) and again after every change, including the ones a
        /// stylesheet causes that no window `resize` event would report
        /// (chrome appearing, a scrollbar vanishing).
        ///
        /// Called once from `set_draw_callback`, which is once per canvas.
        fn observe_css_size(&self) {
            let elem = self.elem.clone();
            let size = self.css_size.clone();
            let draw = self.draw_cb.clone();
            let closure = Closure::<dyn FnMut(js_sys::Array)>::new(move |_entries: js_sys::Array| {
                let rect = elem.get_bounding_client_rect();
                // Skip the notification when the box did not actually change:
                // a redraw of the whole grid is not free, and observing an
                // element reports its size even if nothing moved.
                let new = (rect.width().max(0.0), rect.height().max(0.0));
                if size.get() == new {
                    return;
                }
                size.set(new);
                // Re-enter through the same path a caller's `queue_redraw`
                // takes, so the backing store, the draw callback and the
                // size the caller is told about cannot disagree.
                if let Some(draw_fn) = draw.borrow_mut().as_mut() {
                    let dpr = web_sys::window()
                        .map(|w| w.device_pixel_ratio())
                        .unwrap_or(1.0)
                        .clamp(1.0, 4.0);
                    let dw = ((rect.width() * dpr).ceil() as u32).max(1);
                    let dh = ((rect.height() * dpr).ceil() as u32).max(1);
                    elem.set_width(dw);
                    elem.set_height(dh);
                    if let Some(ctx) = elem
                        .get_context("2d")
                        .ok()
                        .flatten()
                        .and_then(|o| o.dyn_into::<web_sys::CanvasRenderingContext2d>().ok())
                    {
                        // Same device-pixel transform as `queue_redraw`: the
                        // draw callback works in backing-store pixels, and the
                        // context's units are backing-store pixels, so the
                        // scale makes the caller's CSS-pixel geometry come out
                        // at the right physical size.
                        let _ = ctx.set_transform(dpr, 0.0, 0.0, dpr, 0.0, 0.0);
                        let mut dc = WasmDrawContext { ctx, state: RefCell::new(FrameState::default()) };
                        // One save/restore pair per frame, so a clip the caller
                        // set cannot outlive the frame that set it: `clip` only
                        // ever narrows, and `restore` is the DOM's only un-clip.
                        // The caller's `clear` runs its own save/restore with an
                        // identity transform, so a stale clip would otherwise
                        // survive into it and leave everything outside the old
                        // region unpainted.
                        let _ = dc.ctx.save();
                        draw_fn(&mut dc, dw as i32, dh as i32);
                        let _ = dc.ctx.restore();
                        let _ = dc.ctx.set_transform(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);
                    }
                }
            });
            if let Some(win) = web_sys::window() {
                if let Ok(ro) = js_sys::Reflect::get(&win, &wasm_bindgen::JsValue::from_str("ResizeObserver"))
                    .and_then(|c| c.dyn_into::<js_sys::Function>())
                {
                    if let Ok(obj) = js_sys::Reflect::construct(&ro, &js_sys::Array::new()) {
                        let observe = js_sys::Reflect::get(&obj, &wasm_bindgen::JsValue::from_str("observe"))
                            .ok()
                            .and_then(|f| f.dyn_into::<js_sys::Function>().ok());
                        if let Some(observe) = observe {
                            let _ = observe.call1(&obj, self.elem.as_ref());
                        }
                        // The observer must outlive this call: a dropped
                        // `Closure` frees the JS trampoline the observer still
                        // points at. One per canvas, for the life of the page.
                        let _ = js_sys::Reflect::set(
                            &obj,
                            &wasm_bindgen::JsValue::from_str("onresize"),
                            closure.as_ref(),
                        );
                        closure.forget();
                        return;
                    }
                }
            }
            // No ResizeObserver (a pre-2019 engine): size once from the box
            // and leave a later resize to `queue_redraw`, which re-measures.
            let _ = self.sync_backing_store();
        }


        /// Schedule a repaint, coalescing every request made before the next
        /// animation frame into one draw.
        ///
        /// A desktop toolkit works this way: `gtk_widget_queue_draw` only marks
        /// the widget, and the frame arrives later, once. Drawing at the point
        /// of the call is what made the browser build slow -- one keypress asks
        /// for a repaint from every handler it runs through (the window
        /// handler, the canvas handler, the entry, `sync_chrome_labels`, the
        /// status line), and each request was a full grid render: three
        /// 2000-cell frames for one arrow key, each one sent to the compositor.
        /// Coalescing turns that into one.
        pub fn queue_redraw(&self) {
            if self.draw_cb.borrow().is_none() {
                return;
            }
            // Requested while a frame is being painted: it belongs to the
            // frame after it, which `flush_pending_frame` schedules once the
            // paint returns.
            if FRAME_OPEN.with(|f| f.get()) {
                REDRAW_AFTER_FRAME.with(|f| f.set(true));
                self.redraw_pending.set(true);
                return;
            }
            // One of many requests before the frame.
            if self.redraw_pending.get() {
                return;
            }
            self.redraw_pending.set(true);
            let canvas = self.clone();
            let run = move || {
                canvas.redraw_pending.set(false);
                canvas.draw_now();
            };
            let Some(win) = web_sys::window() else {
                // No frame scheduler: draw now, so "drawn when I return" holds.
                run();
                return;
            };
            let cb = Closure::<dyn FnMut()>::new(run);
            if win
                .request_animation_frame(cb.as_ref().unchecked_ref())
                .is_ok()
            {
                // The scheduled frame owns the callback until it runs.
                cb.forget();
                return;
            }
            // `requestAnimationFrame` refuses outside a document: draw now
            // rather than never.
            let canvas = self.clone();
            canvas.redraw_pending.set(false);
            canvas.draw_now();
        }

        /// Draw the current frame, in this turn.
        fn draw_now(&self) {
            if self.draw_cb.borrow().is_none() {
                return;
            }
            let Some(ctx) = self
                .elem
                .get_context("2d")
                .ok()
                .flatten()
                .and_then(|o| o.dyn_into::<web_sys::CanvasRenderingContext2d>().ok())
            else {
                return;
            };
            // The CSS box decides the size; the backing store follows it. See
            // `sync_and_redraw` for why the reset and the draw are one step:
            // assigning `width`/`height` clears the canvas, so painting before
            // that assignment would be erased by it.
            let (w, h) = match self.sync_and_redraw() {
                Some(device) => device,
                None => (self.elem.width() as i32, self.elem.height() as i32),
            };
            // Draw in device pixels: the 2d context's units are backing-store
            // pixels, so the transform maps them back to CSS pixels for a caller
            // that thinks in points (text sizes, hit tests), and a
            // `devicePixelRatio`-aware canvas is crisp rather than upscaled.
            let dpr = web_sys::window()
                .map(|w| w.device_pixel_ratio())
                .unwrap_or(1.0)
                .clamp(1.0, 4.0);
            let _ = ctx.set_transform(dpr, 0.0, 0.0, dpr, 0.0, 0.0);
            let mut dc = WasmDrawContext { ctx, state: RefCell::new(FrameState::default()) };
            // One save/restore pair per frame, so a clip the caller set cannot
            // outlive the frame that set it. The wasm adapter's `clip` narrows
            // the context's clip region, and `restore` is the DOM's only
            // un-clip; without this the next frame's `clear` (which runs its own
            // save/restore with an identity transform) would still be clipped to
            // the previous frame's region.
            let _ = dc.ctx.save();
            // A repaint requested *during* this paint belongs to the next
            // frame: flagging the frame is what stops the app's own
            // "schedule a redraw" (86 call sites, several reached from the
            // draw itself) from being swallowed as "already pending".
            LAST_CANVAS.with(|c| *c.borrow_mut() = Some(self.clone()));
            FRAME_OPEN.with(|f| f.set(true));
            if let Some(draw_fn) = self.draw_cb.borrow_mut().as_mut() {
                draw_fn(&mut dc, w, h);
            }
            FRAME_OPEN.with(|f| f.set(false));
            let _ = dc.ctx.restore();
            let _ = dc.ctx.set_transform(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);
        }

        pub fn set_size_request(&self, w: i32, h: i32) {
            set_css(self.elem.as_ref(), "min-width", &format!("{}px", w.max(0)));
            set_css(self.elem.as_ref(), "min-height", &format!("{}px", h.max(0)));
            // The element also needs a size of its own to fill: a `<canvas>`
            // is a replaced element with an intrinsic 300x150, and inside a
            // flex column that is what it gets unless told otherwise.
            set_css(self.elem.as_ref(), "width", "100%");
            set_css(self.elem.as_ref(), "height", "100%");
            set_css(self.elem.as_ref(), "box-sizing", "border-box");
        }

        /// The size this canvas is drawn at, in backing-store pixels.
        ///
        /// Measured, not remembered. The app keeps its own copy of the canvas
        /// size for hit-testing, and the two have to agree with the surface the
        /// browser will hand the draw callback -- which can change without any
        /// app event (a scrollbar, an orientation change, DevTools docking).
        /// Reading the box here is the only way for the app to notice before
        /// the click that needs it. The box, not the backing store: the grid
        /// is drawn in style pixels (see `local_point`).
        pub fn measure(&self) -> (i32, i32) {
            // The style box. The app hit-tests the pointer against this and
            // draws the grid at this scale; the backing store is that box
            // times `devicePixelRatio` and describes a *surface*, not a
            // layout -- and it is assigned by the first frame, so reading it
            // before then answered 300x150.
            let (css, _device) = self.measure_with_backing_store();
            ((css.0.round().max(1.0)) as i32, (css.1.round().max(1.0)) as i32)
        }

        pub fn set_content_size(&self, w: i32, h: i32) {
            self.elem.set_width(w as u32);
            self.elem.set_height(h as u32);
        }

        pub fn set_visible(&self, v: bool) {
            if v {
                set_css(self.elem.as_ref(), "display", "");
            } else {
                set_css(self.elem.as_ref(), "display", "none");
            }
        }

        /// Button/modifier-aware press: `(x, y, button, modifier mask)`.
        ///
        /// All three of this file's pointer methods were comment-only stubs
        /// ("the DOM handler does not yet forward button/state"), which is
        /// what made canvas dragging, hover and right-click menus impossible
        /// on a page. The DOM has all of it: `MouseEvent::button()` gives the
        /// button number and the shift/ctrl/alt keys are on the event, so the
        /// same `(x, y, button, state)` tuple GTK produces is available here.
        /// The canonical gesture stream: hover, drag, button and scroll in one
        /// vocabulary, built by the backend from the DOM's events. See
        /// `core::Gesture`.
        ///
        /// A browser is a better fit for this than the platform the other two
        /// backends implement it on. NWG has to reach for raw `WM_*` messages
        /// because a `Frame` exposes no mouse API at all, and it recovers the
        /// modifier state from `GET_KEYSTATE` bits. The DOM instead *is* the
        /// unified pointer abstraction: a single `PointerEvent` carries the
        /// button, the set of buttons currently held, and all four modifiers,
        /// for touch, pen and mouse alike. So the mapping is a direct read
        /// rather than a reconstruction.
        ///
        /// Four listeners, because the DOM splits what Win32 packs into one
        /// message:
        ///
        /// * `pointerdown` / `pointerup` -> [`Gesture::Button`]. `PointerEvent`
        ///   has a distinct event for *primary* press and release, but that
        ///   only covers the left button; the others arrive as ordinary
        ///   pointer events, so all buttons are read from `button()`.
        /// * `pointermove` -> [`Gesture::Hover`] with no button held, or
        ///   [`Gesture::Drag`] with one. The DOM already makes that
        ///   distinction — `buttons` is a bitmask — so the test is a mask
        ///   check rather than bookkeeping.
        /// * `wheel` -> [`Gesture::Scroll`].
        ///
        /// Coordinates are the event's `offset_x`/`offset_y`, i.e.
        /// widget-relative, which is what `Gesture` documents and what the
        /// other backends deliver — then scaled from CSS pixels to
        /// backing-store pixels by [`local_point`], so a gesture lands on the
        /// cell it was aimed at on a hi-DPI display or a zoomed page.
        /// `getBoundingClientRect` would give page coordinates, and
        /// subtracting it per event would be a way to be off by a scroll
        /// position.
        ///
        /// Nothing is `prevent_default`ed. The gesture stream is additive
        /// here: this backend's own `on_click` / `on_motion` / `on_release`
        /// are separately registered and may still want the event, and a
        /// wheel handler that swallowed the event would break page scrolling
        /// for a user whose handler ignored it.
        pub fn on_gesture(&self, cb: Box<dyn FnMut(crate::core::Gesture)>) {
            *self.gesture_cb.borrow_mut() = Some(cb);

            // pointerdown / pointerup -> Button
            for (event, pressed) in [("pointerdown", true), ("pointerup", false)] {
                let cell = self.gesture_cb.clone();
                let size = self.css_size.clone();
                let elem = self.elem.clone();
                let closure =
                    Closure::<dyn FnMut(web_sys::PointerEvent)>::new(move |evt: web_sys::PointerEvent| {
                        let (x, y) = local_point(evt.offset_x() as f64, evt.offset_y() as f64, &size, &elem);
                        let gesture = crate::core::Gesture::Button {
                            button: button_from_dom(evt.button()),
                            pressed,
                            x,
                            y,
                            mods: modifiers_from(&evt),
                        };
                        // `try_borrow_mut` so a handler that re-enters (by
                        // queueing a redraw that synthesises an event) drops
                        // the event instead of panicking across the FFI
                        // boundary, where the panic cannot unwind.
                        if let Ok(mut slot) = cell.try_borrow_mut() {
                            if let Some(f) = slot.as_mut() {
                                f(gesture);
                            }
                        }
                    });
                self.elem
                    .add_event_listener_with_callback(event, closure.as_ref().unchecked_ref())
                    .ok();
                self.closures.borrow_mut().push(Box::new(closure));
            }

            // pointermove -> Hover or Drag
            {
                let cell = self.gesture_cb.clone();
                let size = self.css_size.clone();
                let elem = self.elem.clone();
                let closure = Closure::<dyn FnMut(web_sys::PointerEvent)>::new(
                    move |evt: web_sys::PointerEvent| {
                        let mods = modifiers_from(&evt);
                        let (x, y) = local_point(evt.offset_x() as f64, evt.offset_y() as f64, &size, &elem);
                        // `buttons` is a bitmask: 1 = primary, 2 = secondary,
                        // 4 = middle. Any bit set means a drag; none means a
                        // hover. The DOM makes this distinction directly,
                        // where Win32 has to re-read GET_KEYSTATE.
                        let gesture = if evt.buttons() == 0 {
                            crate::core::Gesture::Hover { x, y, mods }
                        } else {
                            crate::core::Gesture::Drag { x, y, mods }
                        };
                        if let Ok(mut slot) = cell.try_borrow_mut() {
                            if let Some(f) = slot.as_mut() {
                                f(gesture);
                            }
                        }
                    },
                );
                self.elem
                    .add_event_listener_with_callback(
                        "pointermove",
                        closure.as_ref().unchecked_ref(),
                    )
                    .ok();
                self.closures.borrow_mut().push(Box::new(closure));
            }

            // `scroll` -> Scroll, for a drag the DOM reports as a scroll.
            //
            // A touchpad two-finger drag and a touch swipe arrive as native
            // `scroll`, not `wheel` (the browser converts the swipe into a
            // scroll of the element under the finger). Without this listener
            // neither moves the sheet. The delta is the *change* in
            // `scrollLeft`/`scrollTop`, so the element does not move and the
            // app receives the same pixels a wheel would have delivered; the
            // position is the element's centre, because a scroll event carries
            // no pointer coordinates of its own.
            {
                let cell = self.gesture_cb.clone();
                let elem = self.elem.clone();
                let last: Rc<Cell<(f64, f64)>> = Rc::new(Cell::new((0.0, 0.0)));
                let size = self.css_size.clone();
                let closure = Closure::<dyn FnMut(Event)>::new(move |_evt: Event| {
                    // Scroll positions round to whole pixels, so a horizontal
                    // scroll that keeps `scrollTop` fixed must report a zero
                    // vertical delta rather than a phantom one.
                    let now = (elem.scroll_left() as f64, elem.scroll_top() as f64);
                    let (lx, ly) = last.get();
                    let (dx, dy) = if now.0 < lx || now.1 < ly {
                        // The element was scrolled programmatically (a
                        // `ScrollIntoView`), not by the user: there is no
                        // gesture to report.
                        (0.0, 0.0)
                    } else {
                        (now.0 - lx, now.1 - ly)
                    };
                    last.set(now);
                    if dx == 0.0 && dy == 0.0 {
                        return;
                    }
                    let cx = elem.client_width() as f64 / 2.0;
                    let cy = elem.client_height() as f64 / 2.0;
                    let gesture = crate::core::Gesture::Scroll {
                        delta: crate::core::ScrollDelta { dx, dy },
                        x: local_point(cx, cy, &size, &elem).0,
                        y: local_point(cx, cy, &size, &elem).1,
                        mods: crate::core::Modifiers::default(),
                    };
                    if let Ok(mut slot) = cell.try_borrow_mut() {
                        if let Some(f) = slot.as_mut() {
                            f(gesture);
                        }
                    }
                });
                self.elem
                    .add_event_listener_with_callback("scroll", closure.as_ref().unchecked_ref())
                    .ok();
                self.closures.borrow_mut().push(Box::new(closure));
            }

            // wheel -> Scroll
            {
                let cell = self.gesture_cb.clone();
                // The element is captured by value: a `&self` cannot be held
                // by a `'static` closure, and the listener must outlive this
                // call. `HtmlCanvasElement` is a JsCast handle, so cloning it
                // is a refcount bump on the same DOM node.
                let elem = self.elem.clone();
                let size = self.css_size.clone();
                let closure =
                    Closure::<dyn FnMut(web_sys::WheelEvent)>::new(move |evt: web_sys::WheelEvent| {
                        // `deltaMode` is 0 for pixels, 1 for lines, 2 for pages.
                        // Only the pixel mode is passed through unconverted: a
                        // line or page delta is multiplied out to a pixel-like
                        // amount so the caller sees one unit, which is what
                        // NWG does by dividing WHEEL_DELTA out of WM_MOUSEWHEEL.
                        // A page is taken as a full viewport height, and a line
                        // as 16 px, the standard conversion.
                        let (dx, dy) = match evt.delta_mode() {
                            1 => (evt.delta_x() * 16.0, evt.delta_y() * 16.0),
                            2 => {
                                // A "page" of scroll is one viewport, so a
                                // page-mode delta is scaled by the canvas's own
                                // height rather than a fixed number.
                                let h = elem.client_height() as f64;
                                (evt.delta_x() * h, evt.delta_y() * h)
                            }
                            _ => (evt.delta_x(), evt.delta_y()),
                        };
                        let gesture = crate::core::Gesture::Scroll {
                            delta: crate::core::ScrollDelta { dx, dy },
                            x: local_point(evt.offset_x() as f64, evt.offset_y() as f64, &size, &elem).0,
                            y: local_point(evt.offset_x() as f64, evt.offset_y() as f64, &size, &elem).1,
                            mods: crate::core::Modifiers {
                                shift: evt.shift_key(),
                                ctrl: evt.ctrl_key(),
                                alt: evt.alt_key(),
                                meta: evt.meta_key(),
                            },
                        };
                        if let Ok(mut slot) = cell.try_borrow_mut() {
                            if let Some(f) = slot.as_mut() {
                                f(gesture);
                            }
                        }
                    });
                self.elem
                    .add_event_listener_with_callback("wheel", closure.as_ref().unchecked_ref())
                    .ok();
                self.closures.borrow_mut().push(Box::new(closure));
            }
        }

        /// Button/modifier-aware press. See the GTK backend's
        pub fn on_click_button(&self, cb: Box<dyn FnMut(f64, f64, u32, u32)>) {
            *self.click_button_cb.borrow_mut() = Some(cb);
            let cell = self.click_button_cb.clone();
            let size = self.css_size.clone();
            let elem = self.elem.clone();
            let closure = Closure::<dyn FnMut(MouseEvent)>::new(move |evt: MouseEvent| {
                if let Some(f) = cell.borrow_mut().as_mut() {
                    f(
                        local_point(evt.offset_x() as f64, evt.offset_y() as f64, &size, &elem).0,
                        local_point(evt.offset_x() as f64, evt.offset_y() as f64, &size, &elem).1,
                        evt.button() as u32,
                        gdk_modifier_mask_mouse(&evt),
                    );
                }
            });
            self.elem
                .add_event_listener_with_callback("mousedown", closure.as_ref().unchecked_ref())
                .ok();
            self.closures.borrow_mut().push(Box::new(closure));
        }

        /// Pointer motion: `(x, y, state mask)`.
        ///
        /// The state is the same GDK-style mask as everywhere else (1 = Shift,
        /// 4 = Control, 8 = Alt), so a caller can ask "is a button held"
        /// without the backend translating a second time.
        pub fn on_motion(&self, cb: Box<dyn FnMut(f64, f64, u32)>) {
            *self.motion_cb.borrow_mut() = Some(cb);
            let cell = self.motion_cb.clone();
            let size = self.css_size.clone();
            let elem = self.elem.clone();
            let closure = Closure::<dyn FnMut(MouseEvent)>::new(move |evt: MouseEvent| {
                if let Some(f) = cell.borrow_mut().as_mut() {
                    f(
                        local_point(evt.offset_x() as f64, evt.offset_y() as f64, &size, &elem).0,
                        local_point(evt.offset_x() as f64, evt.offset_y() as f64, &size, &elem).1,
                        gdk_modifier_mask_mouse(&evt),
                    );
                }
            });
            self.elem
                .add_event_listener_with_callback("mousemove", closure.as_ref().unchecked_ref())
                .ok();
            self.closures.borrow_mut().push(Box::new(closure));
        }

        /// This canvas's top-left in screen coordinates.
        ///
        /// `getBoundingClientRect` gives the viewport-relative box, and the
        /// canvas's offset within the document is added so the answer is in the
        /// same *page* coordinates the GTK backend reports. Returning `None`
        /// here is what forced every context menu on a page to open
        /// unpositioned, since a caller has no origin to place it against.
        pub fn screen_origin(&self) -> Option<(i32, i32)> {
            let rect = self.elem.get_bounding_client_rect();
            let scroll = document().document_element()?;
            let x = rect.x() + scroll.scroll_left() as f64;
            let y = rect.y() + scroll.scroll_top() as f64;
            Some((x.round() as i32, y.round() as i32))
        }

        /// Pointer release: `(x, y, button, modifier mask)`.
        ///
        /// Pairs with [`Canvas::on_click_button`]; a press and a release with
        /// motion between them are what a drag is, and without the release a
        /// drag can never end.
        pub fn on_release(&self, cb: Box<dyn FnMut(f64, f64, u32, u32)>) {
            *self.release_cb.borrow_mut() = Some(cb);
            let cell = self.release_cb.clone();
            let size = self.css_size.clone();
            let elem = self.elem.clone();
            let closure = Closure::<dyn FnMut(MouseEvent)>::new(move |evt: MouseEvent| {
                if let Some(f) = cell.borrow_mut().as_mut() {
                    f(
                        local_point(evt.offset_x() as f64, evt.offset_y() as f64, &size, &elem).0,
                        local_point(evt.offset_x() as f64, evt.offset_y() as f64, &size, &elem).1,
                        evt.button() as u32,
                        gdk_modifier_mask_mouse(&evt),
                    );
                }
            });
            self.elem
                .add_event_listener_with_callback("mouseup", closure.as_ref().unchecked_ref())
                .ok();
            self.closures.borrow_mut().push(Box::new(closure));
        }

        pub fn set_hexpand(&self, expand: bool) {
            if expand {
                set_css(self.elem.as_ref(), "flex-grow", "1");
                set_css(self.elem.as_ref(), "align-self", "stretch");
            } else {
                set_css(self.elem.as_ref(), "flex-grow", "0");
            }
        }

        /// See [`Entry::set_vexpand`].
        pub fn set_vexpand(&self, expand: bool) {
            if expand {
                set_css(self.elem.as_ref(), "flex-shrink", "1");
                set_css(self.elem.as_ref(), "align-self", "stretch");
            } else {
                set_css(self.elem.as_ref(), "flex-shrink", "0");
            }
        }

        /// Outer spacing in px, for a canvas inset inside its container.
        pub fn set_margin_start(&self, px: i32) {
            set_css(self.elem.as_ref(), "margin-left", &format!("{}px", px));
        }

        /// See [`Canvas::set_margin_start`].
        pub fn set_margin_top(&self, px: i32) {
            set_css(self.elem.as_ref(), "margin-top", &format!("{}px", px));
        }

        pub fn on_click(&self, cb: Box<dyn FnMut(f64, f64)>) {
            *self.click_cb.borrow_mut() = Some(cb);
            let cb2 = self.click_cb.clone();
            let size = self.css_size.clone();
            let elem = self.elem.clone();
            let closure = Closure::<dyn FnMut(MouseEvent)>::new(move |evt: MouseEvent| {
                // Take the callback *out* while it runs, and put it back
                // afterwards.
                //
                // Calling it under `borrow_mut` made any re-entry from inside
                // the handler a `RefCell` double-borrow -- a panic that aborts
                // the wasm module, on a plain click. Re-entry is ordinary here:
                // the press handler can open the in-grid aggregate dropdown,
                // which paints and repaints through this same adapter. With
                // the callback moved out, a nested press finds the slot empty
                // and is skipped ("the press is already being handled"), and
                // the outer call restores it on the way out.
                let taken = cb2.borrow_mut().take();
                if let Some(mut f) = taken {
                    let (x, y) = local_point(evt.offset_x() as f64, evt.offset_y() as f64, &size, &elem);
                    f(x, y);
                    *cb2.borrow_mut() = Some(f);
                }
            });
            self.elem
                .add_event_listener_with_callback("mousedown", closure.as_ref().unchecked_ref())
                .ok();
            store_closure(Box::new(closure));
        }

        pub fn on_key(&self, cb: Box<dyn FnMut(u32) -> bool>) {
            *self.key_cb.borrow_mut() = Some(cb);
            let cb2 = self.key_cb.clone();
            let closure = Closure::<dyn FnMut(KeyboardEvent)>::new(move |evt: KeyboardEvent| {
                if let Some(ref mut f) = *cb2.borrow_mut() {
                    if f(evt.key_code()) {
                        evt.prevent_default();
                    }
                }
            });
            self.elem
                .add_event_listener_with_callback("keydown", closure.as_ref().unchecked_ref())
                .ok();
            store_closure(Box::new(closure));
            // Make canvas focusable
            self.elem.set_tab_index(0);
        }
        pub fn on_key_raw(&self, cb: Box<dyn FnMut(u32, u32) -> bool>) {
            // Registered directly rather than through `on_key`, which discards
            // the modifier state -- and a canvas key handler is exactly where
            // an accelerator (Ctrl+S) has to be distinguishable from a bare S.
            *self.key_raw_cb.borrow_mut() = Some(cb);
            let cell = self.key_raw_cb.clone();
            let closure = Closure::<dyn FnMut(KeyboardEvent)>::new(move |evt: KeyboardEvent| {
                if let Some(f) = cell.borrow_mut().as_mut() {
                    if f(evt.key_code() as u32, gdk_modifier_mask(&evt)) {
                        evt.prevent_default();
                    }
                }
            });
            self.elem
                .add_event_listener_with_callback("keydown", closure.as_ref().unchecked_ref())
                .ok();
            self.closures.borrow_mut().push(Box::new(closure));
        }
        pub fn grab_focus(&self) {
            let _ = self.elem.focus();
        }
        pub fn set_can_focus(&self, _can: bool) {}
        pub fn force_draw(&self, _window_ptr: *mut c_void, _fallback_w: i32, _fallback_h: i32) {}
    }

    pub fn create_canvas() -> Result<Canvas, Error> {
        let elem: HtmlCanvasElement = create_element("canvas").dyn_into().map_err(|e| {
            Error::Backend(format!("create_canvas: {:?}", e))
        })?;
        let ctx = elem.get_context("2d")
            .ok().flatten()
            .and_then(|o| o.dyn_into::<web_sys::CanvasRenderingContext2d>().ok())
            .ok_or_else(|| Error::Backend("getContext('2d') failed".into()))?;
        // Use default font that will be overridden per draw call
        let _ = ctx.set_font("12px monospace");
        Ok(Canvas {
            elem,
            draw_cb: Rc::new(RefCell::new(None)),
            click_cb: Rc::new(RefCell::new(None)),
            key_cb: Rc::new(RefCell::new(None)),
            click_button_cb: Rc::new(RefCell::new(None)),
            motion_cb: Rc::new(RefCell::new(None)),
            key_raw_cb: Rc::new(RefCell::new(None)),
            gesture_cb: Rc::new(RefCell::new(None)),
            release_cb: Rc::new(RefCell::new(None)),
            css_size: Rc::new(Cell::new((0.0, 0.0))),
            redraw_pending: Rc::new(Cell::new(false)),
            closures: Rc::new(RefCell::new(Vec::new())),
        })
    }

    // -----------------------------------------------------------------------
    // Overlay (stacking container using position:relative + absolute overlays)
    // -----------------------------------------------------------------------
    pub struct Overlay {
        elem: HtmlDivElement,
        children: Rc<RefCell<Vec<Element>>>,
    }

    impl AsElement for Overlay {
        fn as_element(&self) -> &Element {
            self.elem.as_ref()
        }
    }

    impl Widget for Overlay {
        fn raw_handle(&self) -> *mut c_void {
            &self.elem as *const HtmlDivElement as *mut c_void
        }
    }

    impl Clone for Overlay {
        fn clone(&self) -> Self {
            Overlay {
                elem: self.elem.clone(),
                children: self.children.clone(),
            }
        }
    }

    impl Overlay {
        pub fn set_child(&self, child: &impl AsElement) {
            while let Some(c) = self.elem.first_child() {
                self.elem.remove_child(&c).ok();
            }
            self.children.borrow_mut().clear();
            self.children.borrow_mut().push(child.as_element().clone());
            self.elem.append_child(child.as_element()).ok();
        }

        pub fn add_overlay(&self, child: &impl AsElement) {
            self.children.borrow_mut().push(child.as_element().clone());
            set_css(child.as_element(), "position", "absolute");
            set_css(child.as_element(), "z-index", "10");
            self.elem.append_child(child.as_element()).ok();
        }

        pub fn set_overlay_pass_through(&self, child: &impl AsElement, pass: bool) {
            if pass {
                set_css(child.as_element(), "pointer-events", "none");
            } else {
                set_css(child.as_element(), "pointer-events", "auto");
            }
        }

        pub fn remove(&self, child: &impl AsElement) {
            self.elem.remove_child(child.as_element()).ok();
            self.children.borrow_mut().retain(|c| c != child.as_element());
        }

        pub fn show_all(&self) {
            for c in self.children.borrow().iter() {
                if let Some(html) = c.dyn_ref::<HtmlElement>() {
                    html.style().set_property("display", "").ok();
                }
            }
        }

        pub fn set_size_request(&self, w: i32, h: i32) {
            if w > 0 { set_css(self.elem.as_ref(), "width", &format!("{}px", w)); }
            if h > 0 { set_css(self.elem.as_ref(), "height", &format!("{}px", h)); }
        }

        pub fn set_vexpand(&self, expand: bool) {
            if expand {
                set_css(self.elem.as_ref(), "flex-grow", "1");
            } else {
                set_css(self.elem.as_ref(), "flex-grow", "0");
            }
        }

        pub fn set_hexpand(&self, expand: bool) {
            if expand {
                set_css(self.elem.as_ref(), "align-self", "stretch");
            }
        }

        /// Outer spacing of the overlay itself, in px. See
        /// [`Label::set_margin_start`] for why this is a margin and not a
        /// padding.
        pub fn set_margin_start(&self, px: i32) {
            set_css(self.elem.as_ref(), "margin-left", &format!("{}px", px));
        }

        /// See [`Overlay::set_margin_start`].
        pub fn set_margin_top(&self, px: i32) {
            set_css(self.elem.as_ref(), "margin-top", &format!("{}px", px));
        }
    }

    pub fn create_overlay() -> Result<Overlay, Error> {
        let div: HtmlDivElement = create_element("div").dyn_into().map_err(|e| {
            Error::Backend(format!("create_overlay: {:?}", e))
        })?;
        set_css(div.as_ref(), "position", "relative");
        div.style().set_property("overflow", "hidden").ok();
        Ok(Overlay {
            elem: div,
            children: Rc::new(RefCell::new(Vec::new())),
        })
    }

    // -----------------------------------------------------------------------
    // ScrolledWindow
    // -----------------------------------------------------------------------
    #[derive(Clone)]
    pub struct ScrolledWindow {
        elem: HtmlDivElement,
        /// The registered `on_scroll` callback, if any.
        scroll_cb: Rc<RefCell<Option<Box<dyn FnMut(bool, f64)>>>>,
        /// The scroll listener's own `Closure`, kept alive for as long as the
        /// window is: dropping it would free the JS trampoline the listener
        /// points at, and the next scroll would call into freed memory.
        closures: Rc<RefCell<Vec<Box<dyn Any>>>>,
    }

    impl AsElement for ScrolledWindow {
        fn as_element(&self) -> &Element {
            self.elem.as_ref()
        }
    }

    impl Widget for ScrolledWindow {
        fn raw_handle(&self) -> *mut c_void {
            &self.elem as *const HtmlDivElement as *mut c_void
        }
    }

    impl AsRef<*mut c_void> for ScrolledWindow {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.raw_handle() as *const *mut c_void) }
        }
    }

    impl ScrolledWindow {
        /// Show or hide the native scrollbars.
        ///
        /// GTK's `GtkPolicyType` ordinals: 0 = always, 1 = automatic,
        /// 2 = never, 3 = external. CSS `overflow` has only two answers per
        /// axis -- "there may be a scrollbar" (`auto`/`scroll`) or "there is
        /// none" (`hidden`) -- so *automatic* maps to `auto` (shown only when
        /// needed) and *always* to `scroll` (the bar is reserved even when
        /// there is nothing to scroll). A caller that reserved the space on
        /// purpose gets `scroll`; one that wants it only when needed gets
        /// `auto`. That is the closest honest reading of the four-valued
        /// policy in a two-valued property.
        pub fn set_policy(&self, hscroll: i32, vscroll: i32) {
            let h = match hscroll { 0 => "scroll", 2 | 3 => "hidden", _ => "auto" };
            let v = match vscroll { 0 => "scroll", 2 | 3 => "hidden", _ => "auto" };
            self.elem.style().set_property("overflow-x", h).ok();
            self.elem.style().set_property("overflow-y", v).ok();
        }

        pub fn set_child(&self, child: &impl AsElement) {
            while let Some(c) = self.elem.first_child() {
                self.elem.remove_child(&c).ok();
            }
            self.elem.append_child(child.as_element()).ok();
            // The child is the drawing surface the gesture stream is attached
            // to, and this element is the one that actually scrolls. See
            // `forward_scroll_from` for why the two have to be connected.
            forward_scroll_from(&self.elem, child.as_element());
        }

        /// Push the viewport position and domain into the scroll offset.
        ///
        /// The arguments are the shared **cell-index** model, not pixels:
        /// `sync_scrollbars` (`gui_backend.rs:3053`) passes
        /// `(value, upper, page)` where value and upper count cells. GTK
        /// configures a `GtkAdjustment` with them directly; a DOM element
        /// scrolls in `scrollLeft`/`scrollTop` pixels, so the cell fraction
        /// is scaled by the *scrollable* extent -- which is
        /// `scrollHeight - clientHeight`, not the content height, since
        /// that difference is exactly the distance the offset ranges over.
        ///
        /// A domain of 0, or a container that is not currently scrollable,
        /// is a no-op: there is no position to express.
        pub fn scroll_to(
            &self,
            hval: f64,
            hupper: f64,
            _hpage: f64,
            vval: f64,
            vupper: f64,
            _vpage: f64,
        ) {
            if let Some(x) = self.horizontal_range() {
                if hupper > 0.0 {
                    self.elem.set_scroll_left(clamp_scroll(hval / hupper, x));
                }
            }
            if let Some(y) = self.vertical_range() {
                if vupper > 0.0 {
                    self.elem.set_scroll_top(clamp_scroll(vval / vupper, y));
                }
            }
        }

        /// Fire `cb(vertical, value)` when the user scrolls this element.
        ///
        /// The value is in the same cell-index units `scroll_to` accepts, so
        /// `scroll_to_cursor` (`gui_backend.rs:3064`) can consume it without
        /// a second conversion: the pixel offset divided by the scrollable
        /// extent is the fraction, and the fraction is what the caller wants
        /// (it clamps into the domain itself). GTK reports the adjustment's
        /// value in its own units; here those units *are* the fraction, which
        /// is the same shape.
        pub fn on_scroll(&self, cb: Box<dyn FnMut(bool, f64)>) {
            *self.scroll_cb.borrow_mut() = Some(cb);
            if !self.closures.borrow().is_empty() {
                // A listener is already installed; the closure captured the
                // cell, so it reads the current callback on every event and a
                // second registration needs no second listener.
                return;
            }
            let cell = self.scroll_cb.clone();
            let horizontal_range = {
                let elem = self.elem.clone();
                Rc::new(move || (elem.scroll_width() as i32 - elem.client_width() as i32).max(0))
            };
            let vertical_range = {
                let elem = self.elem.clone();
                Rc::new(move || (elem.scroll_height() as i32 - elem.client_height() as i32).max(0))
            };
            let closure = Closure::<dyn FnMut(Event)>::new(move |evt: Event| {
                let target = match evt.target() {
                    Some(t) => t,
                    None => return,
                };
                let el: Element = match target.dyn_into() {
                    Ok(e) => e,
                    Err(_) => return,
                };
                // A scroll event on a descendant does not bubble here as
                // this element's own offset, so read the element we are
                // attached to rather than the event target.
                let _ = el;
                let y = vertical_range();
                let x = horizontal_range();
                let elem: Element = match evt.current_target().and_then(|t| t.dyn_into().ok()) {
                    Some(e) => e,
                    None => return,
                };
                let mut cb = match cell.try_borrow_mut() {
                    Ok(c) => c,
                    Err(_) => return,
                };
                if let Some(f) = cb.as_mut() {
                    if y > 0 {
                        f(true, (elem.scroll_top() as f64 / y as f64).clamp(0.0, 1.0));
                    }
                    if x > 0 {
                        f(false, (elem.scroll_left() as f64 / x as f64).clamp(0.0, 1.0));
                    }
                }
            });
            self.elem
                .add_event_listener_with_callback("scroll", closure.as_ref().unchecked_ref())
                .ok();
            self.closures.borrow_mut().push(Box::new(closure));
        }

        /// How far the element can be scrolled horizontally, or `None` when
        /// it is not currently scrollable on that axis (which is what makes
        /// `scroll_to` a no-op rather than a jump to 0).
        fn horizontal_range(&self) -> Option<i32> {
            let range = (self.elem.scroll_width() as i32 - self.elem.client_width() as i32).max(0);
            if range > 0 { Some(range) } else { None }
        }

        /// How far the element can be scrolled vertically. See
        /// [`ScrolledWindow::horizontal_range`].
        fn vertical_range(&self) -> Option<i32> {
            let range = (self.elem.scroll_height() as i32 - self.elem.client_height() as i32).max(0);
            if range > 0 { Some(range) } else { None }
        }

        pub fn set_vexpand(&self, expand: bool) {
            if expand {
                set_css(self.elem.as_ref(), "flex-grow", "1");
                set_css(self.elem.as_ref(), "align-self", "stretch");
            } else {
                set_css(self.elem.as_ref(), "flex-grow", "0");
            }
        }

        pub fn set_hexpand(&self, expand: bool) {
            if expand {
                set_css(self.elem.as_ref(), "align-self", "stretch");
            } else {
                set_css(self.elem.as_ref(), "align-self", "");
            }
        }

        /// Size of the *viewport*, in px.
        ///
        /// This is the one place a CSS width is right without a `min-width`
        /// companion: a scroll container's size is its visible box, and the
        /// scrollbar appears inside it rather than growing it. A `min-width`
        /// would fight the `overflow` that makes scrolling work at all.
        pub fn set_size_request(&self, w: i32, h: i32) {
            if w > 0 {
                set_css(self.elem.as_ref(), "width", &format!("{}px", w));
            }
            if h > 0 {
                set_css(self.elem.as_ref(), "height", &format!("{}px", h));
            }
        }
    }

    /// A scroll offset for a fraction of the scrollable range.
    ///
    /// Clamped to the range because `scrollLeft`/`scrollTop` silently ignore
    /// an out-of-range value, and because the caller's fraction can exceed 1
    /// when the domain is larger than what is currently rendered. A NaN
    /// fraction (0/0) is treated as 0, since "no information" and "top left"
    /// are the same answer for a scroll position.
    fn clamp_scroll(fraction: f64, range: i32) -> i32 {
        if !fraction.is_finite() {
            return 0;
        }
        ((fraction.clamp(0.0, 1.0)) * range as f64) as i32
    }

    pub fn create_scrolled_window() -> Result<ScrolledWindow, Error> {
        let div: HtmlDivElement = create_element("div").dyn_into().map_err(|e| {
            Error::Backend(format!("create_scrolled_window: {:?}", e))
        })?;
        div.style().set_property("overflow", "auto").ok();
        div.style().set_property("position", "relative").ok();
        Ok(ScrolledWindow {
            elem: div,
            scroll_cb: Rc::new(RefCell::new(None)),
            closures: Rc::new(RefCell::new(Vec::new())),
        })
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm_adapter::*;
