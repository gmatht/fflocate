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
"RefCell already borrowed"). The 3.x widget API also had no
context-menu, clipboard, drag-and-drop, disclosure, or multi-column-list support
at all, so `gtk_dynamic_loader` gained the missing GTK3/GTK4 bindings, a
closure-backed `ContextMenu`, URI/text drag sources, an `Expander` foldout, a
`TreeView` (with `GValue`-based row insertion and icon-theme loading), and
free-function clipboard helpers. They are vendored rather than referenced by path so the build does not
depend on a checkout outside this repository.

## Status

* Query box + Search button + a status line and a results list, driven by
  `App::run()`.
* The window opens at 1000x700 and the results area expands to fill it; the
  columns are all user-resizable and the Path column absorbs the spare width, so
  it never gets squeezed out as the window grows.
* Results are a **three-column list — Icon / Name / Path**. On GTK this is a
  real `GtkTreeView` backed by a `GtkListStore`, so the columns actually align
  and the icon column shows a real themed icon (picked from the file's name and
  type, with a generic fallback when the theme lacks a specific one). The
  display-free fallback renders the same three columns as aligned text (glyph,
  padded name, path). Clicking blank space below the rows does nothing rather
  than acting on a neighbour; `(no matches)` stays an explicit line.
* Press **Return** in either query field to search — the same code path as the
  Search button, so the two cannot diverge. (In the `zork` fallback, `enter`
  presses Return on the focused field.)
* An **Advanced search** foldout (a native disclosure) holds a free-form query
  field for the full syntax (`ext:`, `path:`, `case:`, `regex:`, globs, phrases)
  together with a reminder of that syntax. Its contents are merged into the
  effective query — one readable string — and when both boxes hold text the
  advanced one runs and the status line says so (live, as you type, and again
  after Search).
* The foldout also carries **working controls**, each backed by a real daemon
  field: *Case sensitive* (`case_sensitive`), *Fuzzy matching* (`fuzzy`) with a
  max-edit-distance field (`max_distance`, greyed out until Fuzzy is ticked),
  and *Count only* (`count`) which returns just the total — shown as
  `N matches (count only)` rather than an empty list that would read as "no
  matches". Ticking them changes what is searched; they are not decoration.
* **Right-click a result** for a context menu: *Copy path*, *Open*, and *Open
  containing folder*. The row under the cursor is found with GTK's own text
  hit-test, so it stays correct under scrolling. Copy uses the system clipboard
  on GTK and the terminal's OSC 52 escape in the `zork` fallback (so it works
  over ssh); Open and Open-containing-folder invoke `xdg-open` on the file and
  its parent directory respectively.
* **Drag a result** into another window. The row is picked up from the pointer
  position at drag start and carried as both a `file://` URI (for a file manager
  or editor) and plain text (for a terminal or text field), so the drop target
  can choose. The `zork` fallback has no other window to drop into; its
  `drag <line>` command prints the payload instead.
* Results come from the daemon's `query` op; the consistency metadata
  (`fresh`, `stale_roots`, `catching_up`) is shown in the status line, so a
  stale view is never presented as an empty one.
* `subscribe` (a live push feed) is still a v1.5 preview in the daemon, so the
  GUI refreshes on demand rather than streaming.
