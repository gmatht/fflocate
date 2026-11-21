# ff-gui

A file-search browser for the fflocate daemon, built on
[rswidgets](https://github.com/) widgets (vendored under `vendor/`).

```sh
cargo run -p ff-gui        # after building, target/debug/ff-gui
```

It talks to `fflocated` over the same NDJSON unix-socket protocol the `ff` CLI
uses: the daemon owns the index, the GUI only collects input and renders rows.

## Backends

On Linux the GUI **defaults to GTK**, which is what a user expects from
`ff --gui`. rswidgets talks to GTK through `gtk_dynamic_loader`, which
`dlopen`s `libgtk-4`/`libgtk-3` at runtime (preferring 4, falling back to 3, and
honouring `GDK_BACKEND`), so nothing is linked at build time.

On macOS the GUI **defaults to AppKit** — a native `NSApplication` window,
driven through the same hand-rolled Objective-C runtime the iOS backend uses
(`vendor/rustxwidgets/src/backends/apple.rs`; no cacao/objc2 dependency).
A plain Rust binary is not an `.app` bundle and never calls
`NSApplicationMain`, so the backend does that work itself: it `dlopen`s AppKit
from libSystem, creates the shared `NSApplication` with the *regular* activation
policy (an accessory app's window can never become key), installs the process's
autorelease pool, builds an `NSWindow` around its content view, calls
`finishLaunching`, and runs `-[NSApplication run]` when it owns the loop.
CI opens that window on a macOS runner and checks the process is still alive
seconds later — the only place this path can be exercised.

The Objective-C shim classes `apple_generator` emits are never compiled here,
so the backend builds them at runtime instead — `objc_allocateClassPair` +
`class_addMethod`, the primitives it already used for its `NSTimer` target:
`CorroMacTarget` (a control's target/action, dispatching into the Rust
callback), `CorroMacMenuTarget` (menu items, in a real `NSMenu` installed with
`setMainMenu:`) and `CorroSheetView` (the canvas: `drawRect:` paints it, text
through `NSString` drawing attributes rather than CoreText, plus mouse clicks).
The `corro*` selectors sent to *stock* views (`corroSetFlex:`,
`corroSetContentHugging:`, `corroSetMinWidth:`, `corroSetPinnedWidth:`) are
added to `NSView` the same way, so they are real behaviour rather than a skip.

The results list is a real `NSScrollView`, the counterpart of
`GtkScrolledWindow`: the canvas is its document view, so it is clipped, has a
scroller and scrolls with the wheel, and `scroll_to`/`set_policy` reach it
instead of being no-ops. `Window::set_child` sizes the root view to the content
view's bounds — AppKit does not lay a plain subview out, and without that the
whole tree sat in a 0x0 box and the window rendered white. `FF_GUI_SCREENSHOT=<path>`
writes a PNG of the window from inside the process, because a CI runner has no
screen to photograph.

The `GUI interactivity (macOS)` CI step proves that rather than asserting it:
it presses the button with `-performClick:` and requires the Rust callback to
fire exactly once, renders the canvas into an `NSBitmapImageRep` and requires a
non-blank image, then builds the real results list with 40 rows and requires a
vertical scroller, content taller than the viewport, a `scroll_to` that moves
the clip view, and a non-blank render of the scrolled list. The screenshot step
attaches the window itself.

Still inert on AppKit: **alerts** (`corroAddAction:` has no runtime
implementation, so `add_button` is a logged skip and a dialog would open with no
buttons; `corroPresentDialog:` is likewise a skip, so `present` only falls back
to `runModal` when no view controller is registered), the canvas' **keyboard**
events (`keyDown:` is not wired), **drag-and-drop**, and the row **context
menu**. Any selector without an implementation is still checked with
`-respondsToSelector:` and the gap logged once rather than aborting the process,
and `$FF_GUI_TRACE=1` names each construction step on stderr.

```sh
cargo run -p ff-gui                       # GTK on Linux, AppKit on macOS
cargo run -p ff-gui --no-default-features --features zork    # terminal fallback
```

The `zork` backend is a display-free driver for the same widget API: it renders
to a REPL rather than a window, so the GUI still runs over ssh or in a container
with no X server. Backends are selected at build time via the crate's
`gtk` / `macos` / `zork` features (`default = ["gtk", "macos"]`; each is inert
off its target). `zork` wins whenever it is enabled, so both
`--no-default-features --features zork` and the additive `--features zork` (on
top of the defaults) build the display-free backend.

### One widget set for both platforms: `rswidgets::compat`

The toolkit this is built on is a vendored rustxWidgets snapshot; when its
backends were merged it dropped `TreeView`, `Expander` and `ContextMenu`, so
those three now come from `rswidgets::compat`, which rebuilds them on the
portable `Canvas` / `Box` / `Label` layer. One implementation therefore serves
GTK and AppKit, and the same code path runs on both — which is what makes this
crate build for Linux and macOS from one source.

What that means in practice, stated rather than implied:

* The results list **draws its own header and rows on a canvas** and hit-tests
  clicks by hand. The three columns line up and a click selects a row (so
  Copy / Open act on the right file), the icon column paints a **pictogram per
  file kind** — folder, source, script, document, image, audio, video,
  spreadsheet, binary, archive, PDF — built from rectangles, since a themed
  pixbuf is not reachable from the portable canvas, and **clicking the Name or
  Path header sorts by it** (a second click reverses it, and the title shows `^`
  or `v`). The icon column does not sort. Row height is fixed. From the repo
  root, `ICON_SHEET=$PWD/icon-sheet.png cargo test --manifest-path
  vendor/rustxwidgets/Cargo.toml --no-default-features --features gtk,macos --lib
  -- icon_sheet` renders every mark to `icon-sheet.png` at the size a row uses.
  The path has to be absolute: `cargo test` runs the test with the vendor
  crate's directory as its working directory, so a relative path lands there.
* **Right-clicking a result opens its row menu.** The row under the cursor is
  selected by the canvas hit-test, then a real menu (`GtkMenu` on GTK, `NSMenu`
  on AppKit) offers *Open*, *Open containing folder*, *Copy full path* and
  *Open in terminal*; each entry dispatches the handler the **Search** menu
  uses, and *Copy full path* puts the row's absolute path on the system
  clipboard, from where it can be read back.
* **Dragging a result out of the list carries its path** on GTK: the canvas owns
  a `text/uri-list` source (plus `UTF8_STRING`), so dropping a row on a terminal
  or a file manager hands it the path. The payload is read when the drop is
  accepted, so it follows the row that is selected then. AppKit has no drag
  source on this layer yet and says so in the status line rather than starting a
  drag that carries nothing.
* The **Advanced search foldout** starts collapsed. On GTK it is the toolkit's
  own `GtkExpander`: its header and arrow are clickable, and the arrow shows the
  state. `View → Advanced Search` toggles it on both desktops; on AppKit, which
  has no foldout on this layer, the header is a label that says `(collapsed)` /
  `(expanded)` and that menu item is the only way to toggle it (AppKit registers
  click handlers in a process-global table that requires `Send`, and a widget
  handle cannot cross a thread). Every control inside it (case, fuzzy, max
  distance, count) works.
* **Wheel stepping** of the max-distance field works where the backend delivers
  wheel events to an entry — GTK does; AppKit and the `zork` REPL report that
  they cannot, which the GUI swallows (the field simply ignores the wheel).
* The timer is per platform: GTK uses `g_timeout_add`, AppKit a delayed-perform
  tick, and the `zork` REPL has none — it returns `Err`, which is why the REPL
  says the pump will not tick rather than silently never firing.

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

While a request is in flight the status line says so — `searching for <query>…
<elapsed>s`, refreshed every 30 ms pump tick, with a live `(N hits so far)` count
parsed from partial reply packets. The daemon currently writes the whole reply at
once, so the count is usually 0 until the answer lands; the elapsed time is the
signal that matters.

When the daemon is mid-crawl (or replaying its journal, or re-verifying), the
line carries **the daemon's own words for it**, read from `Meta` — never a client
guess. `Meta.phase` (`replaying` / `crawling` / `rescanning` / `idle`) plus, while
crawling, `crawl_entries` and `crawl_dirs_pending` render as a bracketed suffix:

```
searching for main… 4.2s  [daemon crawling — 1 234 567 entries, 41 dirs left]
searching for main… 0.3s                       ← idle, caught up: nothing appended
```

A finished search slower than a second keeps its server-side cost on the line
(`1 matches  [2.3s server]`, from `Meta.elapsed_ms`); faster ones stay quiet. The
first search against a cold daemon is the slow one — measured 300–500 ms
`elapsed_ms` on a 2 M-entry live index with a churning queue, versus 0.5 ms on a
settled one — and now it reads as progress, not a frozen window.

> **Why this is not "starting fflocated… _Ns" any more.** That line (until the
> v4 protocol) printed the *client's* process age since it spawned a daemon. The
> number counted up forever and was applied to any query that had not produced a
> Hit yet, so a warm daemon mid-search read as `starting fflocated… 44.0s` — a
> startup that had finished a minute earlier. The daemon had always known better
> (`catching_up`, `lag_ms`), and now says so directly (`phase` + crawl counts).
> An old daemon that sends no `phase` falls back to the `[daemon catching up,
> Ns behind]` wording; the client never invents a cause.

The query is still not *async*. Async needs a loop you own, and the GTK backend
runs glib's own `g_main_loop_run` (`vendor/rustxwidgets/src/backends/gtk.rs:33-38`,
resolved from the dynamic loader and called directly), so there is no executor in
which to `.await`. Reaching that would mean inverting the loop — pumping the glib
context from an executor, the way `asyncio-glib` does in Python — which is a far
larger change to the vendored toolkit than the timer was, and the crate has no
async runtime in its dependency tree. Non-blocking I/O gives the responsiveness
that async was wanted for, without the inversion.

## Startup: paint first, check the daemon second

The window used to verify the daemon *before* presenting: two blocking
round-trips on the warm path, and a spawn plus a blocking up-to-5-second wait
when the daemon was missing — a wedged daemon held first paint hostage
forever. Now `run()` presents first with a `connecting to fflocated…` status and
resolves the check from the same 30 ms pump (`StartupProbe` in `src/lib.rs`):
one `stats` request — which alone proves the daemon is alive, so the old
`hello` round-trip is gone — stepped through `PendingRequest` like any query,
with a single overall deadline bounding the spawn wait too. A missing daemon
is still spawned once, exactly as before; the only change is *when* the answer
lands, never *what* it says.

Measured on this machine (release binary): ~30 ms dlopen + symbol resolution,
~55 ms widget construction (the first widget pays GTK theme initialisation),
and the daemon check moved from blocking first paint to settling 1–2 pump
ticks after it. Against a daemon that accepts and never replies, the window
presents in ~80 ms — previously it never appeared at all.

## Vendored dependencies

`vendor/rustxwidgets` (crate `rswidgets`) and `vendor/gtk_dynamic_loader` are
copies of those crates at the revision corro uses (the `corro_macos_app`
worktree, including its uncommitted AppKit ABI fixes), with the fx additions
re-applied on top. The merged tree has no `Expander`, `TreeView` or
`ContextMenu` — those live in `vendor/rustxwidgets/src/compat.rs`, rebuilt on the
portable layer (see above) — and no drag or icon-loading support at all, so
those calls report their absence. Timer, placeholder text and sensitivity are
per-adapter: the GTK loader gained `gtk_entry_set_placeholder_text`,
`gtk_widget_set_sensitive`, a `scroll-event` connection and the window-icon
pair `gtk_window_set_icon_from_file` / `gtk_window_set_icon_name` for them. Two
fx-only manifest adjustments: the `pdcurses-sys` path dep is dropped (fx never
builds pancurses and cargo resolves optional path deps eagerly), and the
`build.rs` Android resource generator is gated on `RSWIDGETS_ANDROID_PROJECT`
being set (its default root lands inside the vendoring repo).
They are vendored rather than referenced by path so the build does not
depend on a checkout outside this repository.

## Status

* The window carries the **fflocate icon** — the logo with the wordmark
  cropped away (`assets/fflocate.png`, 1024² on transparent, plus the
  `assets/hicolor/` sizes). GTK gets it as its window icon (GTK3 reads the
  512 px file, GTK4 resolves the theme *name*), so the hicolor tree is
  installed under `$XDG_DATA_HOME/icons/hicolor/` before the toolkit starts.
  On macOS the same PNG becomes `NSApplication.applicationIconImage`, i.e. the
  **Dock** icon — a bare Rust binary has no `.app` bundle to carry one. Only
  those two backends have a `set_icon` at all: a Windows icon has to be an
  `HICON` resource and the `zork` fallback is a terminal, so the call site is
  compiled out there instead of every backend carrying a no-op. Launchers and
  app grids read a `.desktop` entry rather than the window, so `prepare()` also
  writes `~/.local/share/applications/fflocate.desktop` — `Icon=fflocate`,
  `StartupWMClass=ff-gui` (the window's `WM_CLASS`) and `Exec=` the binary that
  is running. Only for an installed binary: a `target/` build artifact gets no
  entry, since that path belongs to a working directory rather than an install.
* Query box + Search button + a status line and a results list, driven by
  `App::run()`.
* The window opens at 1000x700 and the results area takes the leftover height. The
  columns are fractions of the list's own width (icon narrow, Name and Path
  sharing the rest), so there is no late `GtkTreeView` header growth to
  compensate for and no one-pixel settle step any more. Column separators do
  not drag: the canvas list has no per-column sizing UI.
* Results are a **three-column list — Icon / Name / Path**, drawn on a canvas
  by `rswidgets::compat::TreeView`, so the columns line up on GTK and AppKit
  alike and a click selects a row. The icon column paints a **pictogram per file
  kind** (folder, source, script, image, archive, …) rather than a themed
  pixbuf, and clicking the Name or Path header sorts by it (again to reverse). Clicking blank space below the rows does
  nothing rather than acting on a neighbour; `(no matches)` stays an explicit
  line. The list is handed its full content height, so more matches than fit
  show the scrolled window's **scrollbar** and scroll with the wheel, while the
  width stays the list's allocation so the columns re-flow with the window. The
  `zork` fallback renders the same three columns as aligned text.

  Clicking a header sorts **every match, not the page on screen**. A search
  asks for 201 hits, so the app then re-asks the same query with `limit = 0` —
  the "no limit" that `ff -l 0` documents — and merges the stream into the
  active sort as it arrives (`TreeView::append_rows`). The status line says what
  happened: `loading all matches: 240 000 of 629 665`, then
  `sorted 629 665 matches`. One unlimited request rather than paging: the
  daemon's per-request cap rescans from the start, so N pages would cost
  O(N²/page) scans and N acquisitions of the index read lock. A second click on
  the same header reverses the order locally — the set is already loaded, so
  nothing is fetched again; a click on a *different* header does re-fetch, since
  the rows kept are the whole set, in the previous order.
  `ff_gui::MAX_SORTED_ROWS` (1 000 000) is the ceiling; past it the line reads
  `stopped at 1 000 000 rows`. A new query, an emptied box or quitting aborts
  the load, keeping whatever rows already arrived.

  **The list is virtualised, so any number of rows scrolls.** The painter
  (`compat::paint_list`) emits only the band inside `[offset, offset+viewport)`,
  in content coordinates, and the canvas is sized to a *screenful*
  (`CANVAS_REQUEST_H`), never to the content — 600 000 rows would be
  12 000 000 px of `GdkWindow`/Cairo surface, far past the ~32 767 px limit.
  Because the canvas no longer carries the content height, the list draws its
  own scrollbar (`reflow::scroll_thumb`: the thumb is the viewport's share of
  the content and reaches the bottom exactly when the offset does) and moves
  itself on the wheel, on a click in the strip and on a thumb drag. Press
  hit-testing adds the offset back, so the row under the pointer is the row
  selected. A backend whose scrolled window still produces a value (macOS's
  `NSScrollView`) also drives the offset through `ScrolledWindow::on_scroll`.
  The wheel needs a canvas scroll signal, which the portable layer has on GTK 3,
  zork, wasm and nwg (`Canvas::on_scroll`, `cfg`'d exactly like
  `on_double_click`); **macOS and GTK4 have none**, so there the list scrolls by
  its scrollbar and thumb drag only — the one place the wheel is missing.

  **The cost is real and is the daemon's.** One such load is one full index
  scan: the index read lock is held for its duration (~1.7 s for 3.6 M entries
  on the reference machine; a regex or fuzzy query is much slower), so journal
  applies wait behind it, and the GUI holds the paths it absorbed (roughly
  150-250 MB at the 1 M-row ceiling — the merged rows, the dedupe set and their
  strings). Painting stays cheap
  whatever the row count: a 600 000-row list emits ~90 draw operations per frame,
  the same as a 30-row one (`reflow_gtk` measures it).
* Press **Return** in either query field to search — the same code path as the
  Search button, so the two cannot diverge. (In the `zork` fallback, `enter`
  presses Return on the focused field.)
* Results also update **as you type**, once typing pauses — see the section below.
  Both the debounce and the button go through the same query code, so a keystroke
  and a button press cannot disagree.
* An **Advanced search** foldout (a native disclosure) holds a free-form query
  field for the full syntax (`ext:`, `path:`, `case:`, `regex:`, `type:`,
  globs, phrases, `-` negation). The field shows that syntax as grey hint
  text while it is empty (GTK and AppKit both support placeholder text);
  the hint disappears as soon as the field holds text or is selected for typing.
  The main search box is literal-only: whatever you type there is quoted, so
  `ext:rs` finds files with those exact characters and `*.rs` finds the
  asterisk too. Type-filter presets (Search → Audio, Folders, …) land in the
  Advanced box, where their `regex:`/`type:` syntax belongs. The Advanced
  field passes through untouched, so the
  effective query stays one readable string — and when both boxes hold text
  the two are ANDed (the quoted Basic term plus the Advanced expression), so
  either box narrows the other. A name containing `"` cannot be quoted; the status line then
  says to use the Advanced box.
* The foldout also carries **working controls**, each backed by a real daemon
  field: *Case sensitive* (`case_sensitive`), *Fuzzy matching* (`fuzzy`) with a
  max-edit-distance field (`max_distance`, greyed out until Fuzzy is ticked),
  and *Count only* (`count`) which returns just the total — shown as
  `N matches (count only)` rather than an empty list that would read as "no
  matches". Ticking them changes what is searched; they are not decoration.
* **Created from / to** in the foldout narrow the list to files whose creation
  time falls in the inclusive `YYYY-MM-DD` range; either end may be left blank.
  Each index entry stores its creation (birth) time as a `u32` of **minutes
  since the epoch** (0 = unknown), captured by the crawl from the same `stat`
  that decides the execute bit, so the daemon sends it with every hit
  (protocol v5) and the range is applied to data that already arrived — no
  per-row `stat`. The GUI filters the rows the daemon returned and the status
  line states what it checked — `[created 2026-01-02–2026-01-03: 12 of 50
  matched]`, plus `N had no creation time` and `N more matches not checked`
  when those apply. A row the daemon could not date (or a daemon older than v5)
  still works: the client falls back to its own `stat` for that row only. A
  date that does not parse, or a `from` after the `to`, is refused in the
  status line instead of being silently ignored.
* **Right-clicking a result opens its row menu.** The row under the cursor is
  selected by the canvas hit-test, then a real menu (`GtkMenu` on GTK, `NSMenu`
  on AppKit) offers *Open*, *Open containing folder*, *Copy full path* and
  *Open in terminal*. Each entry dispatches the same handler the **Search**
  menu uses, for the selected row, and *Copy full path* puts the row's absolute
  path on the system clipboard (`gtk_clipboard_set_text` / `NSPasteboard`),
  from where it can be read back. Both paths are asserted rather than assumed:
  `tests/row_menu_gtk.rs` builds and fires the menu offscreen and under Xvfb,
  and the macOS probe drives the AppKit menu and the pasteboard.
* **Drag-and-drop is unavailable** in this build, and the status line says so
  instead of starting a drag that carries nothing.
* Results come from the daemon's `query` op; the consistency metadata
  (`fresh`, `stale_roots`, `catching_up`, and the daemon's own `phase` with crawl
  progress) is shown in the status line, so a stale view is never presented as an
  empty one and a slow search names what the daemon is actually doing.
* `subscribe` (a live push feed) is still a v1.5 preview in the daemon, so the
  GUI re-queries rather than streaming. As-you-type makes that feel live for the
  index's *contents* changing, but it is still a fresh request each time, not a
  subscription.
