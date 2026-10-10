# DESIGN.md — implementation status

This file records how the implementation maps onto [`DESIGN.md`](DESIGN.md),
where it deviates, and what is deliberately not done. It is not a walkthrough;
it is a status note.

## Implemented

| Design section | Status | Where |
|---|---|---|
| §3 Index data model | done | `crates/findex/src/index.rs`, `roots.rs` |
| §4 Per-mount probing | done | `crates/findex/src/mount.rs` (`fanotify_available`, `handle_of_path`) |
| §4/§6 **exclusions** | done | `crates/findex/src/excludes.rs`, `applier.rs`; tests `tests/excludes.rs` |
| §3/§15 **system daemon** | done | `crates/findex/src/ownership.rs`, `server.rs` (`finish_query`), `crates/findexd/src/net.rs` (`bind_system`, `peer_credentials`); tests `tests/system_access.rs`, `tests/system_live.rs` |
| §5 Initial crawl | done | `crates/findex/src/crawl.rs`, `applier.rs` (`crawl_dir`) |
| §6 Watcher backends | inotify + **fanotify-FID** + fake | `crates/findex/src/watch/{mod,inotify,fanotify,fake}.rs`, `watch/fidmap.rs`; tests `tests/fanotify_live.rs` |
| §7 Reconciliation, backoff, degraded mode | done | `crates/findex/src/pending.rs`, `applier.rs` |
| §8 Cookie sync | done | `crates/findex/src/sync.rs`, `server.rs` (`handle_sync`) |
| §9 Snapshot + journal + cursors + compaction | done | `crates/findex/src/{snapshot,journal,persist,cursor}.rs` |
| §10 Query engine | done | `crates/findex/src/query.rs` |
| §10/§17 **v2 trigram accelerator** | done | `crates/findex/src/trigram.rs`, `query.rs` (`execute_with_trigram`), `applier.rs` (`Shared::trigram_index`); tests `tests/trigram_differential.rs`; bench `examples/trigram_bench.rs` |
| §10 **fuzzy search** | done | `crates/findex/src/fuzzy.rs`, `query.rs` (`execute_fuzzy`), `proto.rs`/`server.rs`/`fx` (`--fuzzy`, `--distance`) |
| §11 IPC protocol v2 | done | `crates/findex/src/proto.rs`, `server.rs` |
| §12 CLI | done | `crates/fx/src/{main,client,service}.rs` (binary `ff`; the `fx` name collides with the snap JSON viewer. Crate dir kept as `crates/fx`.) |
| §13 Concurrency model | done | `crates/findex/src/applier.rs`, `crates/findexd/src/daemon.rs` |
| §14 Testing | done (§14.1–.5) | `crates/findex/tests/*`, `crates/findexd/tests/*`, `scripts/e2e.sh` |
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
  recrawl (`journal.log` present, no snapshot yet; `catching_up` cleared once
  the queue drained).
* Cookie files are cleaned up on completion and on timeout (no leaks).

`cargo test` runs 304 tests, all green, including a 120-step random
differential test against a from-scratch filesystem walk.

Zero-hit `ff` string queries with an uppercase letter get a case-folded
"did you mean" rerun (bounded, stderr only; stdout/exit codes untouched).
That is a *suggestion*; for true edit-distance matching, ranked and on
stdout, use `ff --fuzzy` (§10, above).

### v2 trigram accelerator (§10/§17)

`crates/findex/src/trigram.rs` maps each lowercased-ASCII **name** trigram to a
sorted posting list of entry ids; a query's required literals are intersected
through it and only the survivors are checked. It is a **filter, never an
authority** — `execute_with_trigram` runs the full `Query::matches` on every
candidate, so the accelerator changes only speed. The cache lives on `Shared`,
keyed on `(generation, entry_count)`; any addition or generation bump rebuilds
it, and stale postings that still name tombstoned ids are harmless because the
accelerated path rechecks liveness.

Measured with `cargo run --release -p findex --example trigram_bench -- 1000000`
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

`crates/findex/tests/trigram_differential.rs` pins the filter contract across
random trees and a corpus covering substrings (incl. < 3 bytes), smart-case,
globs, regex, `path:`, phrases, negation and their combinations. The deletion
case is the one that earned its keep: it caught stale postings producing phantom
hits until liveness was rechecked in the accelerated path.

The accelerator is behind a **default-on cargo feature, `trigram`** (`findex`,
with passthroughs on `findexd` and `ff`). `cargo build --no-default-features`
compiles it out entirely — no posting lists, no cache, and queries fall through
to the plain linear scan with identical results. This is the switch to reach for
on memory-constrained machines (the ~4× name-byte cost above) or when the
accelerator is simply not wanted; it is compile-time, so there is no runtime
toggle and no branch cost when off.

### Fuzzy search (§10)

`ff --fuzzy` (or `Request::Query.fuzzy`) turns the plain substring terms into
approximate matches; `--distance N` sets the budget and defaults to **1**. The
matcher (`crates/findex/src/fuzzy.rs`) is bounded, case-insensitive edit
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

`crates/findex/src/watch/fanotify.rs` watches whole filesystems
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
ancestor walk. `FINDEX_EXCLUDE=build,node_modules` sets the rules; empty by
default. Verified live on a fanotify daemon.

### System daemon: privileged, multi-user (§3, §15)

`findexd --system` runs as root, indexes privileged paths, and answers each
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
* **Socket and identity** (`net.rs`): `/run/findexd.sock`, mode 0660 owned by the
  `findex` group; `SO_PEERCRED` for the caller (the kernel's answer, never the
  client's). The daemon refuses to start without the group rather than widening
  the mode.

Verified **live in this container as root, with a real unprivileged user**:
a client outside the `findex` group is refused at connect; one inside sees
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

### Watchman query-language ports (`crates/findex/src/expr.rs`)

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
`scripts/e2e.sh` (`FINDEX_BIN_DIR`) or `--ignored`:

* `crates/findexd/tests/fault_crash.rs` — SIGKILL a live `findexd` mid-churn;
  restart. Plus a **deterministic torn journal tail** (truncate mid-record, which
  is what a real power loss produces; a page-cache SIGKILL on tmpfs rarely
  tears) and **four rounds of repeated `kill -9`**. Every restart comes up,
  reports `journal_corrupt` when torn, never wedges, and converges to filesystem
  truth after a rescan.
* `crates/findexd/tests/fault_fsops.rs` — a real bind mount on a distinct tmpfs,
  unmounted under the daemon; a **mount watchdog** notices the device-id change,
  logs `cause=mount_event`, and reconciles. Also: a root whose path vanishes is
  reported `degraded(root_gone)`, and **watch-limit exhaustion** (lowering
  `fs.inotify.max_user_watches` to 8, restored even on panic) degrades the root
  loudly in `watch_failures`/`watch_limit_hits` with no panic.
* `crates/findex/tests/soak.rs` — a bounded soak: 20 000 random namespace ops
  with live event injection, a full-tree audit every 2 000 ops and a final one,
  asserting **zero drift** and stable survivor ids. It also drives compaction
  (threshold lowered to 1 500 records). `FINDEX_SOAK_OPS` / `FINDEX_SOAK_AUDIT`
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
   change in `crates/findex/src/crawl.rs`.

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

5. **`root_dir_id` matches a root by the basename of its path.** Roots are
   inserted into the index as entries whose name is the final path component.
   Two roots with the same basename would collide. Fine for v1; a root-id field
   on the entry would fix it.

6. **FSEvents is not implemented** (v1.5 in the design). fanotify-FID *is* now
   implemented and live-verified (§6); the `WatchBackend` trait and `BackendCaps`
   are what let it slot in without touching the applier.

7. **The system daemon's ownership sidecar can be stale, by design.** It is
   captured at crawl time and refreshed on rescan; a `chown`/`chmod` between
   rescans is not seen, because fanotify's namespace mask does not report metadata
   changes and adding `FAN_ATTRIB` would make every `touch` an event for no
   benefit to a *name* index. The filter fails closed on a *missing* record, but a
   *stale* one can show a file the caller has since lost access to (or hide one
   they have gained). This is a deliberate trade, recorded here rather than
   discovered later.

8. **Ownership costs an `lstat` per entry, and only the system daemon pays it.**
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
