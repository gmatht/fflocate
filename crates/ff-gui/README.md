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

## As-you-type search over a non-blocking socket

Each change to either query box arms a 150 ms deadline; a 30 ms timer runs the
query once the deadline passes with no further typing, so a burst of keystrokes
collapses to one query. An emptied box clears the list immediately rather than
searching for nothing. `SearchGate` (`src/lib.rs`) refuses to start a query while
one is in flight and drops a result that a newer one has superseded, so the
answers cannot arrive out of order. The Search button and Return go through the
same pump — they just set a flag to skip the debounce — so no path through the GUI
does blocking I/O.

The timer is not part of the original toolkit. `connect_changed` fires
synchronously on every keystroke and there was no timer or idle primitive, so
`App::timeout_add` was added (see **Vendored dependencies** above); the zork
backend, having no timer, returns `Err` and the GUI says so rather than silently
never firing.

**The socket is non-blocking, and a request is *stepped*, never awaited.**
`PendingRequest` (`src/lib.rs`) sets the socket non-blocking, writes the request
line tracking a byte offset (a long expression query can be larger than one
write), and accumulates records until a terminal one arrives. A query's reply is
*multi-record* — a `Meta`, one `Hit` per match, then `Done` — so it arrives in
several packets and the machine has to remember a partial line between them. The
pump calls `advance()` once per tick and treats `WouldBlock` as "come back later",
never as an error.

That is what keeps the window responsive. A **timeout** would not: it bounds the
freeze rather than removing it, so a wedged daemon would still lock the window for
the duration. Non-blocking removes the freeze entirely. Measured against a daemon
that accepts a connection and never replies, the loop regained control **60 times
in 605 ms**; a blocking read would have managed about one.

The query is still not *async*. Async needs a loop you own, and the GTK backend
runs glib's own `g_main_loop_run` (`vendor/rustxwidgets/src/backends/gtk.rs:33-38`,
resolved from the dynamic loader and called directly), so there is no executor in
which to `.await`. Reaching that would mean inverting the loop — pumping the glib
context from an executor, the way `asyncio-glib` does in Python — which is a far
larger change to the vendored toolkit than the timer was, and the crate has no
async runtime in its dependency tree. Non-blocking I/O gives the responsiveness
that async was wanted for, without the inversion.

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
* Results also update **as you type**, once typing pauses — see the section below.
  Both the debounce and the button go through the same query code, so a keystroke
  and a button press cannot disagree.
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
  GUI re-queries rather than streaming. As-you-type makes that feel live for the
  index's *contents* changing, but it is still a fresh request each time, not a
  subscription.
