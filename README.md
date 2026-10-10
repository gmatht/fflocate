# findex

A headless, realtime filename index for Linux: a daemon (`findexd`) and a CLI
(`fx`), speaking NDJSON over a unix socket so a GUI can be built on the same
protocol.

This is an implementation of [`DESIGN.md`](DESIGN.md) (v1 scope).

```
┌───────────┐  unix socket, NDJSON  ┌───────────────────────────────────────┐
│ fx (CLI)  │◄─────────────────────►│                findexd                │
└───────────┘  query/sync/since/    │  watcher (inotify) ─► pending ─► applier│
               subscribe/stats      │                      collection   │     │
                                    │  index (snapshot + replayed journal)   │
                                    └───────────────────────────────────────┘
```

**Single writer, many readers.** Every filesystem event, crawl task and rescan
flows through *one* pending collection owned by one applier thread — one
ordering domain, which is what makes the sync guarantee provable.

## Build

```sh
cargo build --release
# binaries: target/release/findexd and target/release/fx
```

Requires a recent stable Rust (edition 2021). Linux only for the v1 watcher
backend; the core library is portable.

## Quick start

`fx` auto-spawns `findexd` if it is not already running.

```sh
fx main.rs                       # instant search
fx -0 "test" | xargs -0 rg TODO  # pipe-friendly; exit 1 on no match
fx --count ext:pdf
fx --json --limit 1000 "2024" | fzf
fx --sync "notes"                # settle, then search — deterministic
fx --since c:7:481221            # what changed since
fx stats --roots                 # entries, backend, queue depth, caps, state
fx rescan /data
fx update [--timeout 120] [ROOT...]  # reconcile missed changes, then settle
fx watch                         # tail live events (subscribe preview)
fx service install               # systemd user unit / launchd plist
ln -s "$(command -v fx)" ~/bin/locate  # drop-in locate replacement (argv[0])
```

By default the daemon indexes `$HOME`. To index something else:

```sh
findexd --root /path/to/tree --socket "$XDG_RUNTIME_DIR/findex.sock" \
        --state-dir "$XDG_STATE_HOME/findex/2"
```

`fx` finds `findexd` next to itself or on `$PATH` (override with `FINDEXD_BIN`).
`FINDEX_SOCKET` / `FINDEX_STATE_DIR` override the defaults.

## Query syntax

Space-separated terms are ANDed. ASCII-case-insensitive by default, with
smart-case (a term containing an uppercase letter is matched case-sensitively,
like ripgrep/fzf).

| Syntax | Meaning |
|---|---|
| `foo bar` | both substrings present in the name |
| `"foo bar"` | literal phrase |
| `*.rs`, `a?c` | glob over the name |
| `ext:rs` | file extension is `rs` |
| `path:src` | substring anywhere in the materialized path |
| `case:Foo` | force case-sensitive substring |
| `regex:^a.*z$` | full regular expression over the name |
| `-foo` | negation: name must not contain `foo` |

`--count` skips path materialization entirely for a fast count.

## What is implemented (DESIGN.md v1)

* **Index data model (§3)** — 12-byte entries, names arena, no path strings,
  tombstoned deletions, O(1) directory renames, materialization by parent walk.
  Stable ids across rescans; ids are never reused.
* **Per-mount probing (§4)** — fstype from `/proc/self/mountinfo`,
  `name_to_handle_at` probe, writability, case-sensitivity table + temp-file
  probe. Surfaced in `fx stats --roots`.
* **Initial crawl (§5)** — watch first, then enumerate; crawl work flows
  through the same pending collection as watcher events; new directories are
  watched before they are crawled.
* **Watcher backend (§6)** — a `WatchBackend` trait with a real inotify backend
  (one watch per directory; `IN_Q_OVERFLOW` → per-root rescan; `IN_IGNORED` →
  forget wd + rescan; `IN_UNMOUNT` → `RootGone`; rename pairs coalesced; content
  writes ignored) and a fake backend for tests.
* **Reconciliation (§7)** — one pending collection (cookie markers > FIFO
  events > coalesced work items). Rescans **diff** against the index, keep
  survivor ids, and emit synthetic create/delete for genuine differences. A
  full-root rescan bumps the generation.
* **Backoff and degraded mode (§7)** — per-root rescan rate limit (1 s → ×2 →
  5 min cap). Deferred rescans are re-enqueued, never dropped. Roots carry
  `recrawl_count`, `last_rescan_cause`, and `state`.
* **Cookie sync (§8)** — `sync` creates `.findex-cookie-<token>` in each
  writable root; completion requires the marker to be applied *and* the pending
  queues drained. Read-only roots report `"weak"`. Cookies are never indexed nor
  shown to subscribers.
* **Snapshot + journal (§9)** — `FNX2` snapshot with CRC, atomic write, mmap
  load; journal with `[op|len|crc|payload]` framing and bounded torn-tail
  replay; cursors `c:<gen>:<lsn>`; off-lock compaction.
* **Query engine (§10)** — linear scan over the name arena with an
  `aho-corasick` literal prefilter.
* **IPC v2 (§11)** — NDJSON over a unix socket; ops `query`, `sync`, `since`,
  `subscribe`, `stats`, `rescan`, `hello`. Every response's meta carries
  `{gen, lsn, fresh, elapsed_ms, stale_roots[], catching_up, backend, entries}`.
* **CLI (§12)**, **concurrency model (§13)**, **test harness (§14)**,
  **hardening (§15)** — see below.

## Testing

```sh
cargo test          # 122 tests
```

The suite is the design's §14, and it is the point of the project:

1. **Model-based reconciliation** (`crates/findex/tests/reconciliation.rs`) —
   the applier is driven against an in-memory `ModelFs`. We inject event
   sequences that *lie* (missed creates, missed deletes, phantom files, dropped
   watches, overflow) and assert a diff-rescan converges the index to the model
   truth, keeping survivor ids stable.
2. **Real differential tests** (`crates/findex/tests/differential.rs`) — a
   120-step random create/delete/rename/mkdir sequence on a real temp tree; after
   each settle the full index path set is compared against a from-scratch walk.
   Plus backoff rate-limiting and manual-rescan-bypass assertions.
3. **Daemon end-to-end** (`crates/findexd/tests/daemon_e2e.rs`) — real crawl,
   live event application, journal-survives-restart, socket round-trip.
4. **Live kernel inotify** — an inotify test that creates a real file and
   asserts the create event is delivered.

## Layout

```
crates/findex/   core library: index, journal, snapshot, query, applier,
                 pending, watch (inotify + fake), sync, proto, server, mount
crates/findexd/  daemon: config, daemon threads, unix-socket transport, binary
crates/fx/       CLI client + service install
```

## v1.5 / v2 (not implemented)

fanotify-FID and macOS FSEvents backends, trigram accelerator, lazy attribute
sidecar keyed by stable ids, a live GUI subscriber feed. `subscribe` currently
emits the protocol shape (meta + current cursor) as a preview.
