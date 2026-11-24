//! The foldout's *own widgets* must produce a query the daemon answers for a
//! typo: `FreeBiff` typed into the Basic box, *Fuzzy matching* ticked, and the
//! max-distance row left as the app builds it.
//!
//! The sibling `tests/distance_row_{gtk,zork}.rs` prove the row's buttons step
//! the value, and `fflocate`'s `fuzzy_gui_quoting` proves the daemon answers
//! the GUI's quoting. Neither would notice this: a widget that reports the
//! **wrong value** to `SearchOptions` still passes both, while the user sees an
//! empty list. That is the shape of the report this test exists for — on
//! macOS a fuzzy search for a typo came back with nothing — so the whole chain
//! is driven here, with only the socket replaced by a direct `handle_request`:
//!
//! 1. `ff_gui::distance_ui::build` and the foldout's `Fuzzy matching`
//!    checkbox, built the way `main.rs` builds them (`set_active(false)`, the
//!    row seeded with `DEFAULT_MAX_DISTANCE`, the row greyed while Fuzzy is
//!    off);
//! 2. `main.rs`'s decision — tick Fuzzy, wire the row's buttons, arm a search —
//!    then the widgets read back;
//! 3. [`ff_gui::gather_options`] turning them into `SearchOptions`;
//! 4. `DaemonClient::query_request` for `effective_query("FreeBiff", "")` — the
//!    Basic box quotes what is typed — answered by a real `ServerCtx`.
//!
//! Asserted both ways: the default row finds `FreeBuff`, and the failure modes
//! that would produce the empty list are pinned as *failures* — Fuzzy off sends
//! no fuzzy query at all, and a budget of 0 cannot match two edits — so this
//! test says which one a future regression is.
//!
//! AppKit needs the main thread and a window server, and the foldout widgets
//! are created through `App::init`; so, like the other AppKit walks, this has
//! `harness = false` and its own `fn main` (see `[[test]]` in `Cargo.toml`).

#[cfg(all(target_os = "macos", feature = "macos", not(feature = "zork")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::cell::Cell;
    use std::rc::Rc;

    use fflocate::applier::Shared;
    use fflocate::index::{EntryFlags, Index, ROOT_ID};
    use fflocate::proto::{Request, Response};
    use fflocate::roots::Root;
    use fflocate::server::{handle_request, ServerCtx};
    use fflocate::sync::SyncRegistry;
    use ff_gui::{effective_query, gather_options, DaemonClient, SearchOptions};
    use rswidgets::prelude::*;

    // --- A daemon whose root holds the name the typo is aimed at ---
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(dir.path().join("FreeBuff"))?;
    let mut ix = Index::new();
    let base = dir
        .path()
        .file_name()
        .expect("tempdir has a name")
        .to_string_lossy()
        .to_string();
    let rid = ix.insert(
        ROOT_ID,
        base.as_bytes(),
        EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
    );
    ix.insert(rid, b"FreeBuff", EntryFlags::EMPTY.with_dir(true));
    let mut root = Root::new(0, dir.path().to_path_buf());
    root.caps.writable = true;
    let ctx = ServerCtx {
        shared: Shared::new(ix, vec![root]),
        sync: SyncRegistry::new(),
        backend: "fake".into(),
        control: None,
        caller: None,
    };

    /// The one call a search makes on the main thread (`main.rs`'s
    /// `build_request`): the boxes and controls -> the request line.
    fn request_for(query: &str, opts: &SearchOptions) -> Request {
        let q = effective_query(query, "").expect("the Basic box can quote this text");
        DaemonClient::query_request(&q, 0, opts)
    }

    /// The names the daemon answers with, or the error it sent.
    fn ask(ctx: &ServerCtx, req: &Request) -> Result<Vec<String>, String> {
        let mut hits = Vec::new();
        for r in handle_request(ctx, req) {
            match r {
                Response::Hit { path, .. } => hits.push(path),
                Response::Error { message } => return Err(message),
                _ => {}
            }
        }
        Ok(hits)
    }

    // `gather_options` is the library's (it is what `main.rs` calls with the
    // widgets' values), so what this drives is the same mapping the app uses,
    // read from the foldout's real widgets.
    fn options(
        case_on: bool,
        fuzzy: &rswidgets::backends_macos_adapter::CheckButton,
        count_on: bool,
        distance: &ff_gui::distance_ui::DistanceRow,
    ) -> SearchOptions {
        let text = distance.field.get_text().unwrap_or_default();
        gather_options(case_on, fuzzy.is_active(), count_on, &text)
    }

    // --- 1. The foldout as `main.rs` builds it ---
    //
    // Fuzzy starts unticked and the row starts at the app's default, greyed
    // while Fuzzy is off. These are the states the app ships, so this is the
    // starting point a user's window has.
    rswidgets::backends_macos_adapter::ensure_runtime_classes();
    let app = App::init()?;
    let win = app.create_window()?;
    win.set_title("ff-gui foldout fuzzy test");

    let fuzzy = app.create_checkbutton("Fuzzy matching")?;
    fuzzy.set_active(false);
    let distance = ff_gui::distance_ui::build(&app, ff_gui::DEFAULT_MAX_DISTANCE)?;
    distance.set_controls_sensitive(false);

    let advanced_box = app.create_box(Orientation::Vertical, 2)?;
    advanced_box.append(&fuzzy);
    advanced_box.append(&distance.row);
    win.set_child(&advanced_box);
    win.present();

    let field_text = distance
        .field
        .get_text()
        .unwrap_or_else(|| "<none>".to_string());
    println!(
        "foldout: at launch fuzzy {} max distance {field_text:?} (label {:?})",
        fuzzy.is_active(),
        distance.label.get_text().unwrap_or_default()
    );
    assert!(
        !fuzzy.is_active(),
        "the foldout's Fuzzy box starts ticked, which is not what the app ships"
    );
    assert_eq!(
        field_text,
        ff_gui::DEFAULT_MAX_DISTANCE,
        "the max-distance field starts at {field_text:?}, not the app's default: a typo needs \
         one edit, so a field that reads 0 here would explain every empty fuzzy list"
    );

    // --- 2. What a user does: tick Fuzzy, then search the typo ---
    //
    // This is `main.rs`'s handler, in order: the row becomes sensitive, an
    // armed search follows a stepped value, and the boxes are read back through
    // `gather_options`.
    let armed = Rc::new(Cell::new(0usize));
    {
        let armed = armed.clone();
        fuzzy.connect_toggled(move || {
            armed.set(armed.get() + 1);
        })?;
    }
    ff_gui::distance_ui::wire(&distance, {
        let armed = armed.clone();
        move || armed.set(armed.get() + 1)
    });
    let controls = distance.clone();
    let checkbox = fuzzy.clone();
    fuzzy.connect_toggled(move || {
        controls.set_controls_sensitive(checkbox.is_active());
    })?;

    fuzzy.set_active(true);
    let opts = options(false, &fuzzy, false, &distance);
    println!(
        "foldout: after ticking Fuzzy -> active {}, armed {}, options fuzzy {} max distance {:?}",
        fuzzy.is_active(),
        armed.get(),
        opts.fuzzy,
        opts.max_distance
    );
    assert!(
        opts.fuzzy,
        "the ticked Fuzzy box did not reach the request options, so the daemon would run an \
         exact search and `FreeBiff` finds nothing"
    );
    assert_eq!(
        Some(ff_gui::DEFAULT_MAX_DISTANCE.parse::<u32>().unwrap()),
        opts.max_distance,
        "the request carries max_distance {:?} rather than the field's {:?}: a value the daemon \
         reads as 0 cannot match a two-edit typo",
        opts.max_distance,
        field_text
    );

    // --- 3. The daemon answers the request the widgets produced ---
    let hits = ask(&ctx, &request_for("FreeBiff", &opts))?;
    println!("foldout: FreeBiff with the foldout's own values -> {hits:?}");
    assert!(
        hits.iter().any(|p| p.ends_with("/FreeBuff")),
        "the foldout's default state did not find FreeBuff for FreeBiff: {hits:?}"
    );

    // --- 4. The failure modes, pinned as failures ---
    //
    // Each of these is a way the same window can come back empty; naming them
    // here means a future regression reports *which* one came back, instead of
    // just "no matches".
    // Untick the box first: `options` reads the live control, so the request
    // below is only the Fuzzy-off one once the box is off. It was left ticked
    // by the step above, which is what made this assert fail on macOS.
    fuzzy.set_active(false);
    let fuzzy_off = options(false, &fuzzy, false, &distance);
    assert!(
        !fuzzy_off.fuzzy && fuzzy_off.max_distance.is_none(),
        "Fuzzy off must send no fuzzy query and no distance: {fuzzy_off:?}"
    );
    let exact = ask(&ctx, &request_for("FreeBiff", &fuzzy_off))?;
    println!("foldout: Fuzzy unticked -> {exact:?} (an exact search, which is the empty list)");
    assert!(
        exact.is_empty(),
        "an exact search for FreeBiff found {exact:?}: this test can no longer show that \
         unticking Fuzzy is an empty list"
    );

    let zero = SearchOptions {
        fuzzy: true,
        max_distance: Some(0),
        ..Default::default()
    };
    let no_edits = ask(&ctx, &request_for("FreeBiff", &zero))?;
    println!("foldout: budget 0 -> {no_edits:?}");
    assert!(
        no_edits.is_empty(),
        "a budget of 0 matched {no_edits:?}: at 0 nothing is relaxed, so this test can no \
         longer show that a field reading 0 is the empty list"
    );

    // --- 5. The `+` button is the way to a wider budget without a keyboard ---
    //
    // On AppKit the field may take no keystrokes inside a stack view and the
    // backend wires no wheel to an entry, so the buttons are how a macOS user
    // changes this value. `-performClick:` is the click a press sends.
    {
        let press = |btn: *mut std::ffi::c_void| {
            let f: unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) =
                unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
            unsafe { f(btn, rswidgets::backends::apple::selector("performClick:")) }
        };
        let start = distance.field.get_text().unwrap_or_default();
        let before = armed.get();
        press(*distance.increase.as_ref());
        let stepped = distance.field.get_text().unwrap_or_default();
        println!(
            "foldout: `+` stepped {start:?} -> {stepped:?}, armed {} -> {}",
            before,
            armed.get()
        );
        assert_ne!(
            stepped, start,
            "`+` did not change the budget, so on macOS the value cannot be changed"
        );
        assert!(
            armed.get() > before,
            "`+` changed the value but did not arm the search, so the list would not follow it"
        );
        // And the wider budget still finds the name.
        let stepped_opts = options(false, &fuzzy, false, &distance);
        let again = ask(&ctx, &request_for("FreeBiff", &stepped_opts))?;
        assert!(
            again.iter().any(|p| p.ends_with("/FreeBuff")),
            "after `+` the search found {again:?}"
        );
        // Back where it started, so the row is left as the app ships it.
        press(*distance.decrease.as_ref());
    }

    println!(
        "foldout_fuzzy_mac: ok — FreeBiff found FreeBuff with the foldout's own widgets, and \
         Fuzzy off / budget 0 are the only empty lists"
    );
    Ok(())
}

#[cfg(not(all(target_os = "macos", feature = "macos", not(feature = "zork"))))]
fn main() {
    println!("foldout_fuzzy_mac: needs the AppKit backend (macOS, `macos` on, `zork` off)");
}
