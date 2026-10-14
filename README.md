# fflocate

A headless, realtime filename index for Linux: a daemon (`fflocated`) and a CLI
(`ff`), speaking NDJSON over a unix socket so a GUI can be built on the same
protocol.

This is an implementation of [`DESIGN.md`](DESIGN.md) (v1 scope).

```
┌───────────┐  unix socket, NDJSON  ┌───────────────────────────────────────┐
│ ff (CLI)  │◄─────────────────────►│                fflocated                │
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
# binaries: target/release/fflocated and target/release/ff
```

Requires a recent stable Rust (edition 2021). Linux only for the v1 watcher
backend; the core library is portable.

## Quick start

`ff` auto-spawns `fflocated` if it is not already running.

```sh
ff main.rs                       # instant search
ff -0 "test" | xargs -0 rg TODO  # pipe-friendly; exit 1 on no match
ff --count ext:pdf
ff --json --limit 1000 "2024" | fzf
ff --sync "notes"                # settle, then search — deterministic
ff --fuzzy mian.rs               # approximate match (edit distance 1 by default)
ff --fuzzy --distance 2 "notes"  # wider edit budget; results ranked by score
ff --since c:7:481221            # what changed since
ff stats --roots                 # entries, backend, queue depth, caps, state
ff rescan /data
ff update [--timeout 120] [ROOT...]  # reconcile missed changes, then settle
ff watch                         # tail live events (subscribe preview)
ff service install               # systemd user unit / launchd plist
ln -s "$(command -v ff)" ~/bin/locate  # drop-in locate replacement (argv[0])
```

By default the daemon indexes every readable native filesystem on Linux
(`ext4`, `btrfs`, `xfs`, … — each through its own root, so nested mounts are
indexed exactly once and network/pseudo filesystems stay out unless explicitly
asked for with `--root`/`$FFLOCATE_ROOT`); elsewhere it indexes `$HOME`. To
index something else:

```sh
fflocated --root /path/to/tree --socket "$XDG_RUNTIME_DIR/fflocate.sock" \
        --state-dir "$XDG_STATE_HOME/fflocate/2"
```

`ff` finds `fflocated` next to itself or on `$PATH` (override with `FFLOCATED_BIN`).
`FFLOCATE_SOCKET` / `FFLOCATE_STATE_DIR` override the defaults. Dual-boot with a
shared state dir: pass `fflocated --shared-store` so the other OS's database is
adopted (generation bump, local roots rescanned) instead of refused.

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
  probe. Surfaced in `ff stats --roots`.
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
* **Cookie sync (§8)** — `sync` creates `.fflocate-cookie-<token>` in each
  writable root; completion requires the marker to be applied *and* the pending
  queues drained. Read-only roots report `"weak"`. Cookies are never indexed nor
  shown to subscribers.
* **Snapshot + journal (§9)** — `FNX2` snapshot with CRC, atomic write, mmap
  load; journal with `[op|len|crc|payload]` framing and bounded torn-tail
  replay; cursors `c:<gen>:<lsn>`; off-lock compaction.
* **Query engine (§10)** — linear scan over the name arena with an
  `aho-corasick` literal prefilter, plus the **v2 trigram accelerator**
  (plocate-style name posting lists, filter-only, cached by index generation;
  `crates/fflocate/src/trigram.rs`) and **fuzzy search** (`ff --fuzzy`, edit
  distance, ranked by score; `crates/fflocate/src/fuzzy.rs`).
* **IPC v2 (§11)** — NDJSON over a unix socket; ops `query`, `sync`, `since`,
  `subscribe`, `stats`, `rescan`, `hello`. Every response's meta carries
  `{gen, lsn, fresh, elapsed_ms, stale_roots[], catching_up, backend, entries}`.
* **CLI (§12)**, **concurrency model (§13)**, **test harness (§14)**,
  **hardening (§15)** — see below.

## Testing

```sh
cargo test          # 283 tests
```

The suite is the design's §14, and it is the point of the project:

1. **Model-based reconciliation** (`crates/fflocate/tests/reconciliation.rs`) —
   the applier is driven against an in-memory `ModelFs`. We inject event
   sequences that *lie* (missed creates, missed deletes, phantom files, dropped
   watches, overflow) and assert a diff-rescan converges the index to the model
   truth, keeping survivor ids stable.
2. **Real differential tests** (`crates/fflocate/tests/differential.rs`) — a
   120-step random create/delete/rename/mkdir sequence on a real temp tree; after
   each settle the full index path set is compared against a from-scratch walk.
   Plus backoff rate-limiting and manual-rescan-bypass assertions.
3. **Daemon end-to-end** (`crates/fflocated/tests/daemon_e2e.rs`) — real crawl,
   live event application, journal-survives-restart, socket round-trip.
4. **Live kernel inotify** — an inotify test that creates a real file and
   asserts the create event is delivered.

## Layout

```
crates/fflocate/   core library: index, journal, snapshot, query, applier,
                 pending, watch (inotify + fake), sync, proto, server, mount
crates/fflocated/  daemon: config, daemon threads, unix-socket transport, binary
crates/fx/       CLI client + service install
```

## v2

**Done:**

* the **trigram accelerator** (plocate-style name posting lists used as a query
  prefilter). It is filter-only — every candidate is still checked by the full
  matcher, and the differential suite asserts identical results — and its cache
  is keyed by index generation. It lives behind a default-on `trigram` cargo
  feature, so `--no-default-features` builds it out entirely (plain linear scan,
  same results). See `crates/fflocate/src/trigram.rs`,
  `crates/fflocate/tests/trigram_differential.rs`, and
  `cargo run --release -p fflocate --example trigram_bench` (≈6–60× faster queries;
  postings ≈4× the name bytes — the trade the design gates at 10 M+ files).
* **fuzzy search** (`ff --fuzzy`, `--distance N`, default 1 edit). Plain
  substring terms match approximately; `ext:`/`path:`/glob/regex/negation/`case:`
  stay exact. Results are ranked by integer edit distance (then path), so the
  order differs from a plain query even at `k = 0` (same set, ranked sequence).
  The trigram index generates candidates but never decides correctness. See
  `crates/fflocate/src/fuzzy.rs`, `crates/fflocate/src/query.rs` (`execute_fuzzy`).
* **fanotify-FID backend** (§6, v1.5). Whole-mount watch, file-handle events,
  atomic rename pairs; selected by a capability probe (`--backend auto`) with a
  clean fallback to inotify. Verified against real kernel events on a private
  tmpfs. See `crates/fflocate/src/watch/fanotify.rs`, `watch/fidmap.rs`,
  `tests/fanotify_live.rs`.
* **exclusions** (§4/§6) — `FFLOCATE_EXCLUDE=build,node_modules`. A whole-mount
  backend cannot decline a subtree, so the applier filters: an excluded directory
  is never inserted, which prunes its whole subtree.
* **system daemon** (`fflocated --system`, §3). A root-run instance that indexes
  privileged paths and answers each caller as themselves. Per-entry ownership
  (`uid`/`gid`/`mode`) in a sidecar keyed by stable id; queries filtered to what
  the caller may read *including every ancestor*; socket at `/run/fflocated.sock`,
  mode 0660 owned by the `fflocate` group; caller identity from `SO_PEERCRED`.
  Verified live as root with a real unprivileged user. See
  `crates/fflocate/src/ownership.rs`, `crates/fflocate/src/server.rs`,
  `crates/fflocated/src/net.rs`, `tests/system_access.rs`, `tests/system_live.rs`.

**Not implemented:** macOS FSEvents, live `since`/`subscribe` change feeds, and a
GUI client. `subscribe` currently emits the protocol shape (meta + current
cursor) as a preview.
