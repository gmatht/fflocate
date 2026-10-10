# ff-gui

A file-search browser for the findex daemon, built on
[rustxWidgets](https://github.com/) widgets.

```sh
cargo run -p ff-gui        # after building, target/debug/ff-gui
```

It talks to `findexd` over the same NDJSON unix-socket protocol the `ff` CLI
uses: the daemon owns the index, the GUI only collects input and renders rows.

## Why a terminal/headless backend

rustxWidgets has several backends (gtk, nwg, wasm, android, pancurses, zork).
The GUI is written against the shared widget API, but the **only backend that
builds without an external toolchain is `zork`** — the display-free model and
REPL driver. The GTK backends need a gtk4-rs checkout whose `*-dlopen` crates are
generated locally and are not published, so they cannot be built from a clean
checkout.

Using `zork` means `ff-gui` runs in a terminal and over `ssh` with no display
server, and the same widget code drives a real window where GTK is available.

## Vendored dependency

`vendor/rustxwidgets` is a copy of the crate with the fixes needed for its
`zork` backend to compile (`core.rs` had no `zork` arm in its `new_*` wrappers,
and `backends_zork_adapter.rs` was missing a few `Clone` impls and delegate
methods). It is vendored rather than referenced by path so the build does not
depend on a checkout outside this repository. The `gtk` feature and its
`../gtk_dynamic_loader` dependency are removed from the vendored manifest, since
only `zork` is used.

If rustxWidgets upstream lands the equivalent fix, this can go back to a normal
path or crates.io dependency.

## Status

* Query box + Search button + status and results labels, driven by
  `App::run()`.
* Results come from the daemon's `query` op; the consistency metadata
  (`fresh`, `stale_roots`, `catching_up`) is shown in the status line, so a
  stale view is never presented as an empty one.
* `subscribe` (a live push feed) is still a v1.5 preview in the daemon, so the
  GUI refreshes on demand rather than streaming.
