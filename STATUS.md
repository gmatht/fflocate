# DESIGN.md — implementation status

This file records how the implementation maps onto [`DESIGN.md`](DESIGN.md),
where it deviates, and what is deliberately not done. It is not a walkthrough;
it is a status note.

## Implemented

| Design section | Status | Where |
|---|---|---|
| §3 Index data model | done | `crates/fflocate/src/index.rs`, `roots.rs` |
| §4 Per-mount probing | done | `crates/fflocate/src/mount.rs` (`fanotify_available`, `handle_of_path`) |
| §4/§6 **exclusions** | done | `crates/fflocate/src/excludes.rs`, `applier.rs`; tests `tests/excludes.rs` |
| §4 **native-filesystem defaults + mount-aware crawl** | done | `mount.rs` (`NATIVE_FSTYPES`, `default_root_candidates`), `roots.rs` (path-qualified entry names), `applier.rs` (mountpoint set, `may_descend`), `daemon.rs` (covered-root pruning), `config.rs` (`default_roots`); tests `tests/mount_boundary.rs` |
| §3/§15 **system daemon** | done | `crates/fflocate/src/ownership.rs`, `server.rs` (`finish_query`), `crates/fflocated/src/net.rs` (`bind_system`, `peer_credentials`); tests `tests/system_access.rs`, `tests/system_live.rs` |
| §5 Initial crawl | done | `crates/fflocate/src/crawl.rs`, `applier.rs` (`crawl_dir`) |
| §6 Watcher backends | inotify + **fanotify-FID** + fake | `crates/fflocate/src/watch/{mod,inotify,fanotify,fake}.rs`, `watch/fidmap.rs`; tests `tests/fanotify_live.rs` |
| §7 Reconciliation, backoff, degraded mode | done | `crates/fflocate/src/pending.rs`, `applier.rs` |
| §8 Cookie sync | done | `crates/fflocate/src/sync.rs`, `server.rs` (`handle_sync`) |
| §9 Snapshot + journal + cursors + compaction | done | `crates/fflocate/src/{snapshot,journal,persist,cursor}.rs` |
| §10 Query engine | done | `crates/fflocate/src/query.rs` |
| §10/§17 **v2 trigram accelerator** | done | `crates/fflocate/src/trigram.rs`, `query.rs` (`execute_with_trigram`), `applier.rs` (`Shared::trigram_index`); tests `tests/trigram_differential.rs`; bench `examples/trigram_bench.rs` |
| §10 **fuzzy search** | done | `crates/fflocate/src/fuzzy.rs`, `query.rs` (`execute_fuzzy`), `proto.rs`/`server.rs`/`fx` (`--fuzzy`, `--distance`) |
| §11 IPC protocol v2 | done | `crates/fflocate/src/proto.rs`, `server.rs` |
| §12 CLI | done | `crates/fx/src/{main,client,service}.rs` (binary `ff`; the `fx` name collides with the snap JSON viewer. Crate dir kept as `crates/fx`.) |
| §13 Concurrency model | done | `crates/fflocate/src/applier.rs`, `crates/fflocated/src/daemon.rs` |
| §14 Testing | done (§14.1–.5) | `crates/fflocate/tests/*`, `crates/fflocated/tests/*`, `scripts/e2e.sh` |
| §15 Operations and hardening | mostly | `config_paths.rs`, `net.rs`, `health.rs`; `--system` socket/permission handling in `net.rs` |

## Verified behaviour

All of the following were exercised against real binaries and a real temp tree:

* `ff rs`, `ff --count ext:rs`, `ff --json`, `ff -0` — search paths.
* `ff --sync ext:txt` after creating files with **no sleeps** in between;
  all files visible afterwards (deterministic settle barrier).
* **Live inotify**: create a file → visible without `--sync`; rename it (MOVED
  pair) → new name visible, old name gone; delete it → gone. This is real
  kernel inotify, not the fake backend.
* `ff stats --roots` — gen/lsn/entries/tombstones, per-root state/fstype/caps.
* `ff rescan <root>`, `ff --since <cursor>` (cursor is `c:<gen>:<lsn>`).
* Daemon restart: the index came back from **journal replay** without a
  recrawl — the snapshot serves immediately with `catching_up: true` while
  the tail drains in bounded chunks on the applier tick (§9.1); torn tails
  auto-enqueue `JournalCorrupt` rescans and converge without a manual step.
  (`journal.log` present, no snapshot yet; `catching_up` clears once replay
  *and* the initial crawl drain.)
* Cookie files are cleaned up on completion and on timeout (no leaks).

`cargo test` runs 377 tests, all green, including a 120-step random
differential test against a from-scratch filesystem walk.

Zero-hit `ff` string queries with an uppercase letter get a case-folded
"did you mean" rerun (bounded, stderr only; stdout/exit codes untouched).
That is a *suggestion*; for true edit-distance matching, ranked and on
stdout, use `ff --fuzzy` (§10, above).

### v2 trigram accelerator (§10/§17)

`crates/fflocate/src/trigram.rs` maps each lowercased-ASCII **name** trigram to a
sorted posting list of entry ids; a query's required literals are intersected
through it and only the survivors are checked. It is a **filter, never an
authority** — `execute_with_trigram` runs the full `Query::matches` on every
candidate, so the accelerator changes only speed. The cache lives on `Shared`,
keyed on `(generation, entry_count)`; any addition or generation bump rebuilds
it, and stale postings that still name tombstoned ids are harmless because the
accelerated path rechecks liveness.

Measured with `cargo run --release -p fflocate --example trigram_bench -- 1000000`
(1 M entries, this machine):

```
index build:           488 ms
trigram build:        1866 ms  (2024 trigrams, 11.6 M postings)
names arena:          13.1 MiB
trigram postings:     44.4 MiB  (339% of name bytes)

query               matches   linear ms   trigram ms  speedup
very selective          781      275         5.1        54x
word + ext             5663      254        19.7        13x
common word          56 582      252        38.4         7x
common + ext           5703      295        26.6        11x
```

The benchmark asserts accelerated and linear results are identical (count *and*
id order) on every timed query, so a fast number can never come from a wrong
answer. Two honest caveats:

* **Memory ~ quadruples** on the name side (44 MiB of postings vs 13 MiB of
  names; a name averaging ~13 bytes yields ~11.6 trigrams). That is the cost of
  the accelerator and the reason the design gates it at 10 M+ files — at 50 M
  files it would be ~2.2 GB. It is currently built eagerly on first query
  (`Shared::trigram_index`); it is *not* persisted, so a restart rebuilds it
  (1.9 s at 1 M). Both are deliberate v2 scope cuts.
* **`path:` terms are not accelerated** (the index stores no path strings, §3),
  and literals shorter than three bytes carry no trigram; those parts of a query
  fall through to the linear scan. A query that is *only* such terms sees no
  speedup.

`crates/fflocate/tests/trigram_differential.rs` pins the filter contract across
random trees and a corpus covering substrings (incl. < 3 bytes), smart-case,
globs, regex, `path:`, phrases, negation and their combinations. The deletion
case is the one that earned its keep: it caught stale postings producing phantom
hits until liveness was rechecked in the accelerated path.

The accelerator is behind a **default-on cargo feature, `trigram`** (`fflocate`,
with passthroughs on `fflocated` and `ff`). `cargo build --no-default-features`
compiles it out entirely — no posting lists, no cache, and queries fall through
to the plain linear scan with identical results. This is the switch to reach for
on memory-constrained machines (the ~4× name-byte cost above) or when the
accelerator is simply not wanted; it is compile-time, so there is no runtime
toggle and no branch cost when off.

### Fuzzy search (§10)

`ff --fuzzy` (or `Request::Query.fuzzy`) turns the plain substring terms into
approximate matches; `--distance N` sets the budget and defaults to **1**. The
matcher (`crates/fflocate/src/fuzzy.rs`) is bounded, case-insensitive edit
distance: `substring_within` answers "does some substring of the name lie within
`k` edits of the needle" with a free-prefix first DP row and a free-suffix
minimum over the last, falling back to an exact `memmem` find when there is one.
`ext:`, `path:`, glob, regex, negation and `case:` are **not** fuzzed — they
still filter exactly.

Ranking is by the summed per-needle distance (an integer), then path — a total,
deterministic order. One consequence is worth knowing before writing a test:
fuzzy emits a **ranked list**, so its order differs from a plain query's index
order even at `k = 0`, where the two select the same set.

The trigram index's only role is candidate generation, via the same sound bound
the design gives (`t − 3k` shared trigrams); when that bound is vacuous the query
scans everything. Measured at 1 M entries (`trigram_bench`):

```
rare typo   k=1  781 matches   scan 279 ms   trigram  58 ms   4.8x
rare typo   k=2  781 matches   scan 525 ms   trigram 102 ms   5.2x
common typo k=1    0 matches   scan 301 ms   trigram 314 ms   1.0x
common typo k=2  216471 matches scan 985 ms  trigram 1078 ms  0.9x
```

The benchmark asserts accelerated and unaccelerated fuzzy results are identical
in count, ids **and scores**. The honest read of the table: fuzzy helps when the
pattern is long enough (and `k` small enough) for the trigram bound to bite, and
is pure overhead when it is not — a 4-letter pattern at `k = 2` leaves nothing to
constrain. It is opt-in for exactly that reason.

### fanotify-FID backend (§6, v1.5)

`crates/fflocate/src/watch/fanotify.rs` watches whole filesystems
(`FAN_MARK_FILESYSTEM` + `FAN_REPORT_DFID_NAME`) and reports file handles instead
of watch descriptors. `watch/fidmap.rs` maps a handle back to a directory id.
Renames arrive as `FAN_MOVED_FROM`/`FAN_MOVED_TO` and are paired by the moved
object's handle; `FAN_Q_OVERFLOW` escalates to a rescan of every root. Selection
is probed (`mount::fanotify_available()`) and `--backend auto` picks fanotify
when the kernel supports it, else inotify; `ff stats` reports which and why.

Verified **live against real kernel events** on this machine (Linux 6.18 in
WSL2): the test mounts a private tmpfs, learns the root's file handle, and
asserts that create, atomic rename and delete all decode and that they converge
the *index* — not just the event stream. See `tests/fanotify_live.rs`.

Three bugs that only a live test could find, all of which pass a self-consistent
unit test:

* `FAN_REPORT_NAME` is `0x800`, not `0x200`. The wrong value yields *name-less*
  DFID records, so every event is silently lost. The constants are now pinned
  against the kernel header by a test.
* The fd must be non-blocking, or the drain loop hangs forever once the queue
  empties (the first version hung a test for ten minutes).
* A directory created at runtime was indexed but its contents never were: the
  new dir's *file handle* was never learned, so every event naming it as a parent
  was unresolvable. `watch_dir` for this backend is therefore not a no-op — it is
  how the handle gets learned.

### Exclusions under a whole-mount backend (§4/§6)

A whole-mount watch cannot decline a subtree, so the applier filters instead
(`excludes.rs`). An excluded **directory is never inserted**; because events name
their parent by id, that single omission prunes the whole subtree with no
ancestor walk. `FFLOCATE_EXCLUDE=build,node_modules` sets the rules; empty by
default. Verified live on a fanotify daemon.

### System daemon: privileged, multi-user (§3, §15)

`fflocated --system` runs as root, indexes privileged paths, and answers each
caller **as themselves**. The pieces:

* **Ownership sidecar** (`ownership.rs`): `Attr { uid, gid, mode }` per entry id,
  beside the index rather than inside the 12-byte `Entry` — filling it costs an
  `lstat` per file, which the personal mode does not pay. Best-effort across
  rescans (a `chown` is not reported by the namespace mask) and **fails closed**:
  no record ⇒ not shown to anyone but root.
* **The filter** (`server::finish_query`, `visible_chain`): checks the entry *and
  every ancestor*, so a world-readable file inside a `0700` directory stays
  hidden — a per-entry check would get that wrong. All four query shapes (exact,
  fuzzy, expression, count) go through the one function; before it existed, fuzzy
  and expression **leaked**, and the count reported root's total.
* **Socket and identity** (`net.rs`): `/run/fflocated.sock`, mode 0660 owned by the
  `fflocate` group; `SO_PEERCRED` for the caller (the kernel's answer, never the
  client's). The daemon refuses to start without the group rather than widening
  the mode.

Verified **live in this container as root, with a real unprivileged user**:
a client outside the `fflocate` group is refused at connect; one inside sees
`public/readable.txt` and cannot see `private/secret.txt` by name, by fuzzy
search, by expression, or in a count. See `tests/system_access.rs` (the filter's
logic) and `tests/system_live.rs` (the privileged/unprivileged run).

**What could not be verified here**, stated plainly: the live tests need root,
`groupadd`/`useradd` and `setpriv`, and fanotify needs a mountable private tmpfs.
They skip cleanly without those rather than failing, so a run on a restricted
machine exercises less than a run on this one. `SO_PEERCRED` was exercised only
for the same-uid case in the automated test; different-uid connections were
verified by hand with `setpriv`.

### Dual-boot store sharing (`--shared-store`, §15)

One state dir may serve two OSes at different times. Version gate runs first
(newer snapshot → refuse, never recrawl-over); then the `store.meta` stamp
(created/last-writer OS, backend, shared flag, per-root drive map) decides:
foreign without the flag refuses loudly, with it opens as a handoff
(generation bump, `os_handoff` rescans of local roots). Roots are namespaced
by recording OS — foreign roots stay visible but are excluded from queries
(including counts), report `foreign` in sync, and refuse rescan; nothing
deletes them. Drive identity (disk serial + volume UUID, re-probed per boot)
forces a rescan when media swaps under a known path. Covered by
`drive.rs` unit tests (matching, Linux `/dev/disk`+`/sys` fixtures),
`persist.rs` gate-matrix tests, `store_gates.rs` (legacy adopt, newer
refuses, foreign refuses, foreign+flag handoffs end to end), and server
foreign-exclusion tests. Legacy (unstamped) stores adopt unchanged; the
never-compacted foreign store is the one documented unprotected case.

### Native-filesystem defaults: one database, one root per filesystem (§4)

On Linux the daemon now indexes every readable native filesystem by default
(`ext4`, `btrfs`, `xfs`, `f2fs`, …) instead of just `$HOME`; unreadable mounts
are skipped loudly at startup, and when discovery yields nothing usable
(containers) the historical `$HOME` root applies. `$FFLOCATE_ROOT` and `--root`
still win outright, and every other platform keeps `$HOME`.

Deliberately **not** a database per filesystem: the index already stores paths
relative to the root mountpoint (`materialize_into(id, root_path)` — only the
small per-query root table is absolute), so per-filesystem stores would buy no
identity while breaking global cursors, single-writer ordering, and
cross-filesystem queries. What per-filesystem indexing actually needed was:

* **Default discovery** (`mount::default_root_candidates`): native fstypes
  from mountinfo, nested mounts included, duplicates collapsed, sorted.
* **A mount-aware crawl** (`may_descend` in `crawl_dir`/`diff_subtree`): never
  descend into a mount point that is not itself a root, from a mountinfo set
  refreshed on the mount-watchdog tick (zero syscalls on the hot path). The
  mountpoint directory itself is still recorded; a nested mount is covered by
  its own root, and a non-root mount (NFS, pseudo-FS, removable media) never
  leaks in through a parent — which also finally enforces §4's "network FS
  unless opted in" (the fstype exclusion helper existed but was never wired
  into any path). A rescan additionally drops legacy content lingering beneath
  a boundary, healing pre-boundary stores.
* **Root identity by path** (`roots::root_entry_name`): entries are named by
  full path, so same-basename roots (`/data`, `/mnt/data`) cannot collide —
  this fixes old deviation #5 with no format change and no migration. Stores
  written before qualification resolve through a basename fallback, and the
  daemon adopts (never duplicates) a legacy entry.
* **`/` as a root**: the materializer already special-cased a `/` root path;
  only the entry name was missing.
* **Covered-root pruning at open** (`daemon::covered_root_paths`): a stored
  root on the same device strictly below a configured root (yesterday's `$HOME`
  under today's `/home`) is dropped with its subtree, or everything would index
  twice. Only provable coverage prunes — unknown device ids never count.
* **Compaction remaps the root table**: `compact_now` previously remapped the
  ownership sidecar but left `root_dir_ids` pointing at whatever moved into
  the old slots — every watch/crawl/rescan/drop mistargeted after a
  compaction. Covered by a unit test that fails with the remap removed.

Two honest edges, stated plainly: a filesystem remounted at a *different path*
is treated as a new root (recrawl) with the old record degrading loudly — the
storage is already mount-relative, but identity-following adoption is future
work. And the default backend stays inotify: per-filesystem roots make
fanotify-FID (one mark per filesystem) the natural `--backend auto` pick where
privileged, but the default is unchanged.

### GUI menubar (`crates/ff-gui`)

File/Search/View/Help, built through the portable `new_menu` /
`new_simple_action` / `register_action` / `new_menubar` API so every backend
gets it. Every item reuses a handler something else already owns (search
trigger, option widgets, row actions, daemon client); toggles flip their
widgets rather than carrying checkmarks the toolkit cannot render; no
accelerators (the toolkit exposes none). Contents live as testable data
(`menubar_model`: every action exactly once). New client surface this needed:
`DaemonClient::rescan` / `root_paths` (Refresh Index) plus pure
`rescan_request` / `parse_root_paths` constructors. Verified headless: unit
tests plus driving the zork REPL over piped stdin (model renders, Quit
terminates the process). On zork, menu-item selection now dispatches the
item's registered action by name (bare or `app.`-prefixed, as on NWG);
previously only parent-menu callbacks fired.

### `locate` drop-in (`crates/fx/src/locate.rs`)

Invoking the `ff` binary as `locate` (argv\[0\], e.g. a symlink) speaks GNU
locate: substring/`*PATTERN*` semantics, full-path shell globs (`*` crosses
`/`, translated to anchored `pcre`), `-r/--regex`, `-b`, `-i`, `-A`
(anyof/allof), `-c`, `-l`, `-0`, `-e` (with `-L`/`-P` symlink handling),
`-q`, `-S` (maps to `ff stats`), `-d` accepted and ignored — the live index
replaces the updatedb snapshot. Same exit statuses (0/1/2). Covered by unit
tests (parsing, glob→regex) and `crates/fx/tests/locate_e2e.rs` (symlinked
binary against a real daemon, incl. the dangling-symlink `-e`/`-L` case),
wired into `scripts/e2e.sh`.

### Watchman query-language ports (`crates/fflocate/src/expr.rs`)

The expression-tree language (`since.cpp` term added: `ClockSpec` with
timestamp/`c:pid:ticks`/`c:start:pid:root:ticks` clockspecs, `oclock`/
`cclock`/`mtime`/`ctime` fields, Watchman's `>=`/`>` boundary semantics and
error strings) plus the ported suites: the full 208-row
`WildmatchTest.json` table, `test_empty.py`, `test_pcre.py`,
`test_since_term.py`, `SuffixQueryTest.cpp` (`simple_suffix_type()` /
`suffix_glob_patterns()`), and the `relative_root` / case-sensitivity /
path-depth cases from `test_name/match/glob/suffix_generator/
path_generator.py`. Porting the wildmatch table fixed three divergences from
`wildmatch.c`: `**` outside a segment boundary is malformed (fails the
match), POSIX `[:class:]` support, and the `/.` period rule for `**`.

### Fault injection and soak (§14.3, §14.5)

These spawn real binaries and need kernel capabilities, so they live behind
`scripts/e2e.sh` (`FFLOCATE_BIN_DIR`) or `--ignored`:

* `crates/fflocated/tests/fault_crash.rs` — SIGKILL a live `fflocated` mid-churn;
  restart. Plus a **deterministic torn journal tail** (truncate mid-record, which
  is what a real power loss produces; a page-cache SIGKILL on tmpfs rarely
  tears) and **four rounds of repeated `kill -9`**. Every restart comes up,
  reports `journal_corrupt` when torn, never wedges, and converges to filesystem
  truth after a rescan.
* `crates/fflocated/tests/fault_fsops.rs` — a real bind mount on a distinct tmpfs,
  unmounted under the daemon; a **mount watchdog** notices the device-id change,
  logs `cause=mount_event`, and reconciles. Also: a root whose path vanishes is
  reported `degraded(root_gone)`, and **watch-limit exhaustion** (lowering
  `fs.inotify.max_user_watches` to 8, restored even on panic) degrades the root
  loudly in `watch_failures`/`watch_limit_hits` with no panic.
* `crates/fflocate/tests/soak.rs` — a bounded soak: 20 000 random namespace ops
  with live event injection, a full-tree audit every 2 000 ops and a final one,
  asserting **zero drift** and stable survivor ids. It also drives compaction
  (threshold lowered to 1 500 records). `FFLOCATE_SOAK_OPS` / `FFLOCATE_SOAK_AUDIT`
  raise the budget for a long run; `#[ignore]`d so `cargo test` stays quick.

### An important finding

**`IN_UNMOUNT` is not delivered on this platform (WSL2)** — verified with a
standalone inotify probe: unmounting produces no event on either the mount point
or a subdirectory inside it. The design's `IN_UNMOUNT → RootGone` therefore
cannot be the only mount-detection mechanism, so the daemon gained a portable
**mount watchdog** (compare each root's `st_dev` periodically). The backend still
handles `IN_UNMOUNT` where it is delivered.

## Deviations from the design

These are deliberate and small; each is noted so the design stays honest.

1. **Crawl uses `std::fs::read_dir`, not a hand-rolled `getdents64`.** The
   performance target (§17: ≥ 1 M files/s/core) is not met by this path. The
   `DT_UNKNOWN` → `fstatat` fallback is expressed as a capability (§4) and
   probed, but the hot loop is the stdlib one. Replacing it is a contained
   change in `crates/fflocate/src/crawl.rs`.

2. **`subscribe` is a preview.** Per §11 it is "v1.5". The op is accepted and
   answers with meta + the current cursor, but there is no live per-subscriber
   feed yet. `ff watch` therefore tails one snapshot, not a stream.

3. **`since` emits coarse deltas.** The change log stores cursors at which the
   index changed, not per-path records, so `--since <cursor>` reports matching
   paths from the *current* index plus flush markers, rather than the exact
   create/delete deltas between two cursors. A stale generation correctly returns
   `fresh: true`. Making it exact needs a per-path change journal (§9's journal
   already has the records; the mapping id→path is the missing piece).

4. **Path resolution goes through `String::from_utf8_lossy` for real
   directories.** The index itself stores raw bytes and non-UTF-8 names are
   round-tripped through the journal and snapshot intact; only the directory→
   absolute-path cache used to *reach* the filesystem during crawl/diff lossy-
   converts. A non-UTF-8 directory name is still indexed correctly, but a rescan
   of its subtree may not find it on disk.

5. **FSEvents is not implemented** (v1.5 in the design). fanotify-FID *is* now
   implemented and live-verified (§6); the `WatchBackend` trait and `BackendCaps`
   are what let it slot in without touching the applier.

6. **The system daemon's ownership sidecar can be stale, by design.** It is
   captured at crawl time and refreshed on rescan; a `chown`/`chmod` between
   rescans is not seen, because fanotify's namespace mask does not report metadata
   changes and adding `FAN_ATTRIB` would make every `touch` an event for no
   benefit to a *name* index. The filter fails closed on a *missing* record, but a
   *stale* one can show a file the caller has since lost access to (or hide one
   they have gained). This is a deliberate trade, recorded here rather than
   discovered later.

7. **Ownership costs an `lstat` per entry, and only the system daemon pays it.**
   `Applier::track_ownership` is off unless `--system`. A personal index crawls
   with no stats at all, which is why the sidecar is not simply part of the
   normal path.

## Known gaps / bugs found and fixed during the build

* Cookie registry was keyed by root *index* but looked up by root *dir id*;
  fixed to key both on the dir id. This was caught by the `--sync` reporting
  `weak` on a writable root.
* The applier tick drained events before control messages, so a cookie's create
  event could arrive before the cookie was registered. Fixed by handling control
  first.
* Cookie files leaked on the last sync because `cookie_path` was read after
  `complete` had already forgotten the token. Fixed to read the path first.
* Deferred rescans (backoff) were silently dropped. Fixed to re-enqueue; the
  drain loop stops on a deferral so it does not spin.
* `since` never emitted a terminal response, hanging the client. Fixed.
* `FsEvent::RootGone` carried a *root index*, but the inotify backend can only
  know a *directory id* — so an unmount dropped the wrong thing (or nothing).
  Changed the event to carry the dir id; the applier drops that subtree.
* A failed `watch_dir` was silently swallowed (`let _ = ...`), so
  watch-limit exhaustion produced no degradation and no counter. Now the watcher
  thread reports failures to the applier, which marks the root degraded and bumps
  `watch_failures`/`watch_limit_hits` — loudly stale, not silently wrong.
* **Startup compaction + LSN epoch reset.** A journal tail replayed at boot never
  counted toward `records_since_compaction` (it bypasses `emit`), so a large
  inherited tail — 672k records / 26 MB in the live incident — replayed on
  *every* boot. `Daemon::build` now folds a clean tail past the threshold into a
  fresh snapshot once. This exposed a latent correctness bug: `compact_now`
  wrote the old LSN high-water into the new snapshot while `Journal::truncate`
  reset the journal clock to 0, so next boot's `lsn > last_lsn` replay skip
  would have silently discarded every post-compact write. Compaction now resets
  the epoch across snapshot, shared clock, and journal together (the generation
  bump already invalidates old `c:gen:lsn` cursors).
* **Watcher↔applier ABBA deadlock (root-caused from a live wedge).** Both
  threads parked forever in futex waits — stacks recovered from the wedged
  process via `/proc/PID/mem` + `addr2line` against `/proc/PID/exe`: main in
  `tick → watch_dir → watch_tx.send` (watch channel, cap 4096), the watcher in
  `tx.send` of event batches (cap 1000). Each side's blocking send is the
  other's consume step: one long applier work item (a full-root
  `diff_subtree` — `DRAIN_TIME_BUDGET` paces *between* items, not within one)
  starves the event-channel drain; a live event flood (two concurrent build
  sessions on indexed roots) fills it; the parked watcher stops servicing watch
  requests; the applier's flush — one request per surviving directory after a
  full-tree pass, far more than 4096 — blocks in `watch_dir` and main never
  returns to the top of its tick. A later SIGTERM then made the accept loop drop
  the listener while the wedged applier never cleaned the socket or pidfile:
  clients saw ECONNREFUSED (111) against a held pidfile — the "deaf daemon".
  Fixed in the watcher: event batches are sent with a 50 ms deadline, watch
  requests are serviced between retries, and the stop flag is re-checked on
  every retry; watch-failure reports are `try_send` (dropped loudly to the
  daemon log when saturated) so they cannot re-create the cycle. Neither events
  nor watch requests are ever dropped, and shutdown stays prompt. Pinned by
  `watcher_services_watch_requests_while_events_back_up` (30 s watchdog — the
  plain blocking-send variant hangs at exactly that watchdog).
fails at exactly that watchdog).
* **Deaf daemon: the accept loop is now supervised.** The incident above showed
  the deeper hole: the accept thread was fire-and-forget, so a live process
  (pidfile held, applier ticking, watcher polling) could go deaf with nobody
  left to notice — the client could only fail fast (`spawn::deaf_daemon_message`)
  and tell the operator to kill it. Now an `AcceptSupervisor`
  (`crates/fflocated/src/net.rs`) owns the listener: it adopts the startup bind,
  and re-binds + respawns on death with a 1 s backoff. The supervisor is shared
  (`Arc<Mutex<..>>`) and ticked from *both* the applier loop (`main`) and the
  watcher loop (`Daemon::start`, ~20×/s), so either surviving thread revives the
  listener — which covers the wedged-applier case above, where only the watcher
  is alive to notice. Each repair bumps `health.accept_restarts` (visible in
  `ff stats`; 0 is healthy), and `serve_client` no longer panics on a failed
  stream clone. Pinned by `net::supervisor_tests::supervisor_respawns_a_dead_accept_loop`
  (kill the adopted generation, tick, assert `accept_restarts == 1` and a live
  `Hello` round-trip on the same socket path). One honest scope bound: if the
  applier thread itself is gone, serving is broken regardless (it owns all index
  mutation), so a resurrected listener would be theater — and a minutes-long
  `diff_subtree` delays the applier-side tick until the drain returns, bounding
  the deaf window by the longest single work item, not unbounded.
* **Dead-client scan halt.** A client that disconnects mid-scan used to cost a
  full scan to a dead socket: the socket `emit` in `net::respond_streaming`
  already swallowed the write error into a `closed` flag, but the scan kept
  running to `Done` (which itself failed silently). Now two mechanisms compose:
  (1) every `emit` in the chain returns `bool` — `query::consider_emit` + the
  three `*_emit` engines (`Option<u64>`: `None` on abort, since a partial total
  would lie in `Done`), the four `server::handle_*_streaming` fns (no `Done`
  after an abort), and the socket closure (refuses once closed); (2) an active
  disconnect poll — `query::ScanAbort` (`Option<&dyn Fn() -> bool>`), threaded
  through all engines and checked per 4096-entry chunk (~4 ms of scan), so the
  worst overshoot after a disconnect is one chunk. The poll closes the three
  holes passive write-error detection leaves: zero-hit scans (no writes, no
  errors), kernel-buffered writes (succeed long after the peer vanished), and
  fuzzy compute (scores the whole candidate set before the first emit — hence
  `execute_fuzzy_abortable`, with `execute_fuzzy` delegating abort-free).
  `net::client_gone_fd` implements the poll on unix (zero-timeout `poll` for
  `POLLIN|POLLHUP|POLLERR` + `MSG_PEEK`: 0 = FIN, error = RST, bytes = a
  pipelined request, left untouched); Windows keeps write-error-only halt (no
  `poll`+peek without going nonblocking, which the serve loop needs blocking).
  Batched `handle_request` and all collect-then-filter paths pass `None`, so
  their wire shape is unchanged. Pinned by `query::emit_refusal_aborts_the_scan`,
  `query::expr_emit_refusal_aborts_the_scan`,
  `query::abort_poll_stops_a_zero_hit_scan`,
  `query::abort_poll_stops_fuzzy_mid_compute`,
  `server::streaming_abort_skips_done_and_stops_the_scan`,
  `server::streaming_abort_poll_stops_the_scan_without_any_refusal`, and
  `net::client_gone_detects_fin_but_not_data` (socketpair: live/pipelined alive,
  FIN gone, peek consumes nothing).
* **Daemon-emitted scan-fraction progress.** Slow GUI searches used to show
  only elapsed seconds (`searching for comp… 44s`) with no sense of how much
  scan remained — the client knew hits-so-far but neither the scan fraction
  nor the total (`Done` comes last), and `hits/entries` sits at ~0.08% for a
  query like `comp`, which is noise, not progress. Now an opt-in
  `Query.progress` flag (GUI only; `ff` never sends it, so its output is
  byte-identical) asks the daemon to emit `Progress { scanned, total }`
  records — entries *visited* vs entries to visit — between `Meta` and
  `Done`, on all four scan paths (linear, trigram, fuzzy scoring, expr).
  The engines report per 4096-entry chunk through a single `ScanEvent`
  callback (one `&mut` borrow: `Hit` and `Progress` share it, since they emit
  interleaved); the server rate-gates to ~10/s (`PROGRESS_MIN_INTERVAL`) plus
  the initial `(0, total)` denominator announcement, so fast queries send 0–2
  lines and the percent simply never appears for them. The GUI renders
  `searching for comp… 42%` / `7 hits… (counting total… 1.2s, 3%)` from a
  per-ticket monotone watermark (capped at 99 — 100% is `Done`'s privilege),
  dropped on supersede like the partial-render count. No version bump: the
  flag is an unknown JSON *field* to old daemons (serde ignores it) and
  Progress-free replies render seconds-only, exactly as before. Pinned by
  `query::progress_reports_scan_fraction_then_completes`,
  `query::progress_refusal_aborts_with_no_total`,
  `server::progress_records_flow_between_meta_and_done`,
  `daemon_e2e::progress_opt_in_streams_scan_fraction_over_the_socket`, and
  the GUI's `scan_percent_*` / `progress_percent_*` / `query_request_opts_*`
  / `outcome_ignores_progress_records` tests; the `streaming_lockorder`
  wire-shape assert allows (never requires) Progress between Meta and Done.
  Verified live against a release daemon: a 3k-entry opt-in query yields
  `Meta → Progress(0/3000) → Hits → Done`, opt-out yields no Progress, and
  `ff` output is unchanged. Honest scope bound: fuzzy percent covers the
  *scoring* pass (candidates visited), not the ranking that follows — and a
  sparse trigram candidate set finishes in 1–2 chunks, so percent jumps
  rather than glides. Both are the scan's real work, not estimates.
* **Daemon lag, in seconds.** `[daemon catching up]` said *that* the daemon
  was behind, never *how far* — the honest half of the warning was missing.
  Now every queued change (watcher event, crawl, rescan) is stamped with its
  arrival wall time in `pending::Pending` (`Stamped<T>`), the applier tick
  mirrors the oldest still-unapplied stamp into `health.oldest_queued_at_ms`
  (`0` = caught up, and a still-draining journal-replay tail counts as
  unapplied work with `pending_replay_started_ms` as its floor), and
  `meta_preamble` computes the *age at reply time* as `Meta.lag_ms`
  (saturating, so a clock step cannot underflow into a colossal number).
  Clients render it as seconds: the GUI shows
  `1 matches  [daemon catching up, 12s behind]` and the in-flight lines too
  (`searching for comp… 1.3s 42%  [daemon catching up, 7s behind]`), `ff`
  warns `daemon is still catching up (4s behind)`, and `ff stats` prints a
  derived `lag  12.3s behind (oldest unapplied change)`. Honest bounds, all
  deliberate: the age is measured from *queue arrival*, so it is the
  daemon's own backlog age, not a filesystem-wide clock skew measure;
  seconds round down and a sub-second lag keeps the old bare
  `[daemon catching up]` (a number would claim precision the clock lacks);
  ancestry collapse inherits the *oldest* member's stamp rather than
  resetting to `now`, so a rescan storm cannot hide lag; a coalesced
  re-enqueue keeps the original stamp for the same reason; and an older
  daemon that omits `lag_ms` renders exactly the previous wording. Pinned by
  `pending::tests::oldest_queued_*` / `coalesced_reenqueue_keeps_original_stamp`,
  `server::tests::meta_reports_lag_age_when_work_is_queued` (age at reply
  time, saturation, the `0` sentinel), `proto::tests::response_lines_roundtrip`
  (present when behind, absent when caught up, an old client line parses as
  `None`), and the GUI's `catching_up_suffix_*` / `*_line_shows_lag_seconds` /
  `outcome_from_records_carries_lag`. Verified live against a real daemon:
  a churning queue reports a rising age, and a settled one omits the field.

## Reproducing the checks

```sh
cargo build --release
cargo test
# a live demo, real inotify:
#   see the transcript in the commit/PR message, or run the daemon + ff by hand
```

## Honest health

`ff stats` is the operational surface. If a root is degraded, a rescan was
deferred, the journal was corrupt, or the daemon is still catching up, it is
visible there — the design's "loudly stale beats silently wrong" rule.

`ff update [--timeout 120] [ROOT...]` is the manual update (the updatedb
equivalent): rescan every root, then settle. A rescan enqueued before the
sync cookie cannot complete after it (pending ordering), so a root reporting
`ok` is reconciled with the filesystem — this heals downtime/offline edits
a restart crawl alone cannot (crawls only add). Exit 1 if any root is not
`ok`; unknown roots are an error. Covered by `crates/fx/tests/update_e2e.rs`
(kill -9 → offline delete+add → restart → update converges, with
`last_rescan_cause=manual` proving the update's rescan did it).

## Windows port (`x86_64-pc-windows-msvc`, merged to main)

Validated on real Windows 11 hardware: full `cargo test --workspace` green,
plus the release-binary e2e suites (`locate_e2e`, `update_e2e`, `fault_crash`,
`fault_fsops` — the last two skip only what needs bind mounts/inotify
sysctls). Findings from the bring-up, all fixed:

* **IPC**: `fflocate::transport` (`Stream`/`Listener` aliases + `connect`);
  unix sockets on unix, TCP loopback on Windows. The socket "path" stays a
  `PathBuf` everywhere: on Windows it carries `127.0.0.1:PORT` text
  (`FFLOCATE_PORT`, default 18923). No new dependencies.
* **Paths**: index stays `/`-canonical; `util::normalize_separators` runs at
  root ingestion (`Root::new`), `handle_rescan`, and the `ff` sends, so
  `C:\tree` keys and matches. Non-UTF-8 names go lossy (documented).
* **Files/defaults**: state under `LOCALAPPDATA`/`APPDATA`, home default from
  `USERPROFILE`, pidfile without `flock` (the TCP bind is the exclusion),
  no signal handlers (Ctrl-C kills; the journal replays), default backend
  `fake` (ReadDirectoryChangesW is future work).
* **Sync without a watcher**: the fake backend polls watched dirs for cookie
  files and synthesizes their create events, so `sync`/`update` complete;
  `ok` means *quiescent*, not *caught up* — `ff update` (rescan + sync) is
  the sound operation there. Covered by a unit test proving the
  emit → marker → completion chain.
* **Deferred**: real watcher backend, stable file IDs (`windows-sys`
  `GetFileInformationByHandle`), service installation (`ff service install`
  errors), `find -user/-group/-perm/-inum/-links` (loud errors, never
  silent), case-insensitivity defaults to the writable-dir probe.
* **Tests**: e2e harnesses take OS-assigned loopback ports (`test_socket`)
  and `.exe`-aware binary names; `locate` tests copy to `locate.exe`
  (argv[0] accepts the `.exe` form); `Child::kill` everywhere (also fixing
  a Windows hang in the old wait-only harness kill); OS-vs-index path
  comparisons go through a `canon()` helper (verbatim prefixes stripped,
  `/`-normalized).
* **Winsock gotcha (the big one)**: accepted sockets inherit the listener's
  nonblocking mode on Windows (opposite of POSIX). The nonblocking accept
  loop therefore handed `serve_client` sockets whose first idle read failed
  with WouldBlock → instant close → RST — killing every second request on a
  reused connection (`ff --sync` always died). Fixed by forcing accepted
  streams blocking in `net::accept_loop` (and the `daemon_e2e` direct
  accept); harmless on unix.
* **Root-gone detection**: the mount watchdog was purely device-id based,
  and device IDs are unavailable on Windows — a vanished root never alarmed.
  The watchdog now also alarms on a missing path (identical outcomes
  elsewhere: a gone root already probed `None` there).
* **Flakes fixed along the way**: the cookie-cleanup assertion now polls
  instead of asserting once (complete-then-unlink races the check under
  load); `resolve_bases` treats rooted paths as absolute and strips
  verbatim `\\?\` prefixes so `ff C:\path` filters match.
* **Windows e2e costs**: socket waits are file-existence polls that always
  run the full 10 s (sockets aren't files there) — suites take ~30 s each;
  a TCP-probe wait would fix that. 9P mtime granularity can make cargo skip
  rebuilds of just-synced sources — touch before building there.
