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
Unlike GTK, AppKit cannot be `dlopen`d: it needs a host that owns the
`NSApplication` event loop, so `App::run()` hands off to it rather than
blocking.

```sh
cargo run -p ff-gui                       # GTK on Linux, AppKit on macOS
cargo run -p ff-gui --no-default-features --features zork    # terminal fallback
```

The `zork` backend is a display-free driver for the same widget API: it renders
to a REPL rather than a window, so the GUI still runs over ssh or in a container
with no X server. Backends are selected at build time via the crate's
`gtk` / `macos` / `zork` features (`default = ["gtk", "macos"]`; each is inert
off its target).

### macOS v1 scope

The AppKit port is functional but deliberately narrower than GTK: the results
list is a text-backed `TreeView` (rows render as `name  path` lines with path
selection, but no click-to-sort headers or hit-testing yet), the context menu
logs instead of popping an `NSMenu`, the foldout is inert, and the timer is a
delayed-perform tick rather than an `NSTimer`. A real `NSTableView`,
`NSMenu`, disclosure and canvas port is future work; every stub degrades
loudly (status line / log), never silently.

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
re-applied on top. The upstream tree has no `Expander`, `TreeView`,
`ContextMenu`, timer, clipboard, placeholder, drag, or icon-loading support —
those are all fx additions, now merged into the new tree across every backend
(gtk real, zork headless, nwg/wasm/android/pancurses/ios/macos stubs). Two
fx-only manifest adjustments: the `pdcurses-sys` path dep is dropped (fx never
builds pancurses and cargo resolves optional path deps eagerly), and the
`build.rs` Android resource generator is gated on `RSWIDGETS_ANDROID_PROJECT`
being set (its default root lands inside the vendoring repo).
They are vendored rather than referenced by path so the build does not
depend on a checkout outside this repository.

## Status

* Query box + Search button + a status line and a results list, driven by
  `App::run()`.
* The window opens at 1000x700 and the results area expands to fill it; every
  column header separator drags (all columns are user-resizable) and the Path
  column absorbs the spare width, so it never gets squeezed out as the window
  grows. Name defaults to 250 px so filenames read instead of ellipsizing to
  stubs. The default screen never needs a horizontal scrollbar: the fixed Name
  default plus the expanding Path hold widths steady (long paths ellipsize
  with "…" instead of forcing the bar — FIXED sizing is not the mechanism, it
  would kill the draggable dividers), and the window steps out from 1 px
  narrow once settled so the headers' late growth cannot leave it wider than
  the glass.
* Results are a **three-column list — Icon / Name / Path**. On GTK this is a
  real `GtkTreeView` backed by a `GtkListStore`, so the columns actually align
  and the icon column shows a real themed icon. **Click a header
  to sort by it** (GTK sorts the store and draws the header arrow): Name and
  Path sort by themselves, the icon column sorts by file extension. (picked from the file's name and
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
  field for the full syntax (`ext:`, `path:`, `case:`, `regex:`, `type:`,
  globs, phrases, `-` negation). The field shows that syntax as grey hint
  text while it is empty and unfocused;
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
* **Right-click a result** for a context menu: *Copy path*, *Open*, and *Open
  containing folder*. The row under the cursor is found with GTK's own text
  hit-test, so it stays correct under scrolling. Copy uses the system clipboard
  on GTK and the terminal's OSC 52 escape in the `zork` fallback (so it works
  over ssh); Open and Open-containing-folder invoke `xdg-open` on the file and
  its parent directory respectively.
* **Drag a result** into another window. The row is picked up from the pointer
  position at drag start and carried as both a `file://` URI (for a file manager
  or editor) and plain text (for a terminal or text field), so the drop target
  can choose. While dragging, the row's own themed icon rides under the cursor
  (via `drag-begin`), so the picked-up item stays visible in flight. The `zork`
  fallback has no other window to drop into; its
  `drag <line>` command prints the payload instead. Note: the drop target must
  speak XDND — xterm does not (no `XdndAware`), so drops into xterm silently go
  nowhere from any application; Kitty and other XDND-aware terminals work.
* Results come from the daemon's `query` op; the consistency metadata
  (`fresh`, `stale_roots`, `catching_up`, and the daemon's own `phase` with crawl
  progress) is shown in the status line, so a stale view is never presented as an
  empty one and a slow search names what the daemon is actually doing.
* `subscribe` (a live push feed) is still a v1.5 preview in the daemon, so the
  GUI re-queries rather than streaming. As-you-type makes that feel live for the
  index's *contents* changing, but it is still a fresh request each time, not a
  subscription.
