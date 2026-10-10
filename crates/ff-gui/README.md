# ff-gui

A file-search browser for the findex daemon, built on
[rustxWidgets](https://github.com/) widgets.

```sh
cargo run -p ff-gui        # after building, target/debug/ff-gui
```

It talks to `findexd` over the same NDJSON unix-socket protocol the `ff` CLI
uses: the daemon owns the index, the GUI only collects input and renders rows.

## Backends

On Linux the GUI **defaults to GTK**, which is what a user expects from
`ff --gui`. rustxWidgets talks to GTK through `gtk_dynamic_loader`, which
`dlopen`s `libgtk-4`/`libgtk-3` at runtime (preferring 4, falling back to 3, and
honouring `GDK_BACKEND`), so nothing is linked at build time.

```sh
cargo run -p ff-gui                       # GTK (default)
cargo run -p ff-gui --no-default-features --features zork    # terminal fallback
```

The `zork` backend is a display-free driver for the same widget API: it renders
to a REPL rather than a window, so the GUI still runs over ssh or in a container
with no X server. Both backends are selected at build time via the crate's
`gtk` / `zork` features.

## Vendored dependencies

`vendor/rustxwidgets` and `vendor/gtk_dynamic_loader` are copies of those crates
with the fixes needed for the `zork` backend to run `ff-gui` (`core.rs` had no
`zork` arm in its `new_*` wrappers; `backends_zork_adapter.rs` was missing a few
`Clone` impls and delegate methods; and `backends/zork.rs` invoked widget
callbacks while it still held the state `RefCell`'s `borrow_mut`, so a button
like Search — whose callback calls back into the widget API — panicked with
"RefCell already borrowed"). They are vendored rather than referenced by path so
the build does not depend on a checkout outside this repository.

## Status

* Query box + Search button + a status line and a results list, driven by
  `App::run()`.
* Results render into a `TextView`, one path per row — a real scrollable list
  rather than one wrapping paragraph. `(no matches)` is shown as an explicit
  single line so it is never confused with an empty/absent daemon.
* Results come from the daemon's `query` op; the consistency metadata
  (`fresh`, `stale_roots`, `catching_up`) is shown in the status line, so a
  stale view is never presented as an empty one.
* `subscribe` (a live push feed) is still a v1.5 preview in the daemon, so the
  GUI refreshes on demand rather than streaming.
