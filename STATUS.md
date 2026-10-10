# DESIGN.md — implementation status (v1)

This file records how the implementation maps onto [`DESIGN.md`](DESIGN.md),
where it deviates, and what is deliberately not done. It is not a walkthrough;
it is a status note.

## Implemented

| Design section | Status | Where |
|---|---|---|
| §3 Index data model | done | `crates/findex/src/index.rs`, `roots.rs` |
| §4 Per-mount probing | done | `crates/findex/src/mount.rs` |
| §5 Initial crawl | done | `crates/findex/src/crawl.rs`, `applier.rs` (`crawl_dir`) |
| §6 Watcher backends | inotify + fake | `crates/findex/src/watch/{mod,inotify,fake}.rs` |
| §7 Reconciliation, backoff, degraded mode | done | `crates/findex/src/pending.rs`, `applier.rs` |
| §8 Cookie sync | done | `crates/findex/src/sync.rs`, `server.rs` (`handle_sync`) |
| §9 Snapshot + journal + cursors + compaction | done | `crates/findex/src/{snapshot,journal,persist,cursor}.rs` |
| §10 Query engine | done | `crates/findex/src/query.rs` |
| §11 IPC protocol v2 | done | `crates/findex/src/proto.rs`, `server.rs` |
| §12 CLI | done | `crates/fx/src/{main,client,service}.rs` |
| §13 Concurrency model | done | `crates/findex/src/applier.rs`, `crates/findexd/src/daemon.rs` |
| §14 Testing | done (§14.1–.5) | `crates/findex/tests/*`, `crates/findexd/tests/*`, `scripts/e2e.sh` |
| §15 Operations and hardening | mostly | `config_paths.rs`, `net.rs`, `health.rs` |

## Verified behaviour

All of the following were exercised against real binaries and a real temp tree:

* `fx rs`, `fx --count ext:rs`, `fx --json`, `fx -0` — search paths.
* `fx --sync ext:txt` after creating files with **no sleeps** in between;
  all files visible afterwards (deterministic settle barrier).
* **Live inotify**: create a file → visible without `--sync`; rename it (MOVED
  pair) → new name visible, old name gone; delete it → gone. This is real
  kernel inotify, not the fake backend.
* `fx stats --roots` — gen/lsn/entries/tombstones, per-root state/fstype/caps.
* `fx rescan <root>`, `fx --since <cursor>` (cursor is `c:<gen>:<lsn>`).
* Daemon restart: the index came back from **journal replay** without a
  recrawl (`journal.log` present, no snapshot yet; `catching_up` cleared once
  the queue drained).
* Cookie files are cleaned up on completion and on timeout (no leaks).

`cargo test` runs 158 tests, all green, including a 120-step random
differential test against a from-scratch filesystem walk.

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
   feed yet. `fx watch` therefore tails one snapshot, not a stream.

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

6. **fanotify-FID and FSEvents are not implemented** (v1.5 in the design). The
   `WatchBackend` trait and `BackendCaps` are in place for them.

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
#   see the transcript in the commit/PR message, or run the daemon + fx by hand
```

## Honest health

`fx stats` is the operational surface. If a root is degraded, a rescan was
deferred, the journal was corrupt, or the daemon is still catching up, it is
visible there — the design's "loudly stale beats silently wrong" rule.
