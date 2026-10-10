# findex — Design Document (v2)

Headless, realtime filename index for Linux (macOS later). Daemon (`findexd`) + CLI (`ff`), GUI-ready via IPC. This revision integrates the Watchman lessons: reconciliation as a first-class subsystem, cookie-based sync, journal-as-clock, per-mount probing, and health telemetry. Changes from v1 are summarized in §0; everything else is the complete standalone design.

---

## 0. What changed from v1

| # | Lesson (source) | Design change |
|---|---|---|
| 1 | Reconciliation is core, not error handling (Watchman) | New §7: pending collection, diff-based rescans, backoff, degraded mode |
| 2 | Cookies give a settle barrier (Watchman) | New §8: `sync` op; `ff --sync`; deterministic tests |
| 3 | The journal is the clock (Watchman `since`) | §9/§11: LSN cursors, `--since`, subscription-ready protocol, `fresh` semantics |
| 4 | Rescans must diff, not replace (Watchman) | Entry IDs stable across rescans (§3, §7) |
| 5 | Probe the FS, don't assume (Watchman) | §4: per-mount capability probing, per-root flags |
| 6 | Operational scars (Watchman/RN ecosystem) | §15: health counters, rescan causes, backoff, state-dir versioning |
| 7 | Watchman's fake watcher backend | §14: test harness with injectable backend |
| 8 | FSearch: monitor toggle per index location | §4: per-root watch enable/disable in config |

---

## 1. Goals and non-goals

**Goals**

- Machine-wide name index; query p95 < 20 ms at 10 M files.
- Live updates with a *provable* convergence story (sync op), not best-effort.
- Crash-safe: snapshot + journal; restart in < 50 ms without full recrawl.
- Scriptable CLI; protocol that a GUI can build on without changes.
- Honest health reporting: never silently stale.

**Non-goals (v1)**

- Content search. Content writes generate zero index work; only namespace ops (create/delete/rename) matter.
- Per-file metadata (size/mtime) in the core index — lazy sidecar in v2.
- Triggers/run-on-change (v3 at earliest), project-root scoping (that's Watchman's model, not ours).

---

## 2. Architecture

```
┌───────────┐  unix socket, NDJSON  ┌────────────────────────────────────────────┐
│ ff (CLI)  │◄─────────────────────►│                  findexd                   │
│ GUI later │  query/sync/since/    │                                            │
└───────────┘  subscribe/stats      │  watcher backend (fanotify|inotify|FSEvents)│
                                    │        │ FsEvent                           │
                                    │        ▼                                   │
                                    │  ┌───────────────────┐   crawl/rescan work │
                                    │  │ pending collection│◄──────────────┐    │
                                    │  │ (cookie > events  │               │    │
                                    │  │  > events > work) │               │    │
                                    │  └─────────┬─────────┘               │    │
                                    │            ▼                         │    │
                                    │  ┌───────────────────┐    ┌───────────┴──┐ │
                                    │  │ applier (single   │───►│ crawler /    │ │
                                    │  │ writer) + journal │    │ rescanner    │ │
                                    │  └─────────┬─────────┘    └──────────────┘ │
                                    │            ▼                               │
                                    │  ┌───────────────────┐                     │
                                    │  │ index (mmap snap  │◄── query engine ──┐ │
                                    │  │ + replayed journal│    (RwLock read)  │ │
                                    │  └───────────────────┘                   │ │
                                    └──────────────────────────────────────────┼─┘
                                                                               ▼
                                                                          clients
```

Single writer (applier), many readers. All FS events, crawl tasks, and rescan tasks flow through **one** pending collection — one ordering domain, which is what makes the sync guarantee (§8) provable.

---

## 3. Index data model

Unchanged core from v1:

```rust
struct Entry {          // 12 bytes
    name_off: u32,      // offset into names arena
    name_len: u16,
    parent: u32,        // ROOT = 0
    flags: u8,          // IS_DIR | IS_SYMLINK | TOMBSTONE
}
```

No path strings stored — materialized by parent-chain walk on demand, so directory renames are O(1). Deletions are tombstones; path materialization drops results through tombstoned dirs; compaction purges. Non-UTF-8 names stored as raw bytes. Budget ≈ 35 B/file → 10 M files ≈ 400 MB.

**v2 additions:**

- **Stable IDs across rescans.** IDs are never reused, and a rescan (§7) matches surviving entries by `(parent_id, name)` and keeps their IDs. Anything consuming IDs (future attr sidecar, subscribers) can rely on identity.
- **`generation: u64`** on the index. Bumped on: snapshot load, full-root rescan, compaction rebuild. A generation bump invalidates all cursors (§9) — subscribers resync with `fresh: true` rather than applying deltas of uncertain provenance.
- **Roots table** in the index header:

```rust
struct Root {
    path: PathBuf,
    dev: u64,
    enabled: bool,          // per-location watch toggle (FSearch lesson)
    caps: MountCaps,        // §4
    state: Ok | Degraded{reason} | Rescanning,
    last_full_crawl: Cursor,
}
```

---

## 4. Per-mount probing and configuration

At root-add (and on mount events), classify the mount from `/proc/self/mountinfo`, then **probe** rather than assume:

| Probe | Method | Decides |
|---|---|---|
| File handles supported | `name_to_handle_at` on the root dir | fanotify-FID backend usable on this mount |
| dtype reliability | `getdents64` returns `DT_UNKNOWN`? | whether `fstatat` fallback is needed during crawls |
| Case sensitivity | fstype table (vfat/exfat/ntfs → insensitive); optional temp-file-pair probe when the dir is writable | match semantics per root |
| Writable | `access(W_OK)` | cookie/sync support (§8) |

Exclusions: pseudo-FS (`proc`, `sysfs`, `devtmpfs`, `cgroup*`, `tracefs`, `overlay`), network FS unless opted in, user config excludes. Per-location watch enable/disable mirrors FSearch's monitor toggle.

Surface everything via `ff stats --roots` — a mis-probed mount should be visible, not mysterious.

---

## 5. Initial crawl

As v1, unchanged in essentials:

- **Watch first, then enumerate** (the race rule). During crawl, incoming events are queued in the pending collection and applied after; all applies are idempotent upserts, so replay is safe.
- Raw `getdents64`, 64 KB buffer, no `stat` (dtype → is_dir; `fstatat` only on `DT_UNKNOWN`). Never follow symlinks. Parallel work-stealing over discovered dirs; IDs from a central atomic counter.
- `(st_dev, st_ino)` set over visited **directories** defeats bind-mount loops.
- New in v2: crawl tasks are enqueued through the same pending collection as watcher events (one ordering domain), and each crawled dir is recorded `enumerated`. The post-crawl reconciliation pass (§7) re-enumerates any dir that received events after its own enumeration — usually zero or a handful of dirs.

---

## 6. Watcher backends

One internal event language, backends are pluggable (a future kernel API — fsnotify pre-content events, EdenFS-style ideas — becomes a new backend, not a rewrite):

```rust
enum FsEvent {
    Upsert { parent: DirKey, name: Bytes, is_dir: bool },
    Remove { parent: DirKey, name: Bytes },
    RescanSubtree { dir: DirKey, cause: RescanCause },
    RootGone { root: RootId },
}

trait WatchBackend {
    fn add_root(&mut self, root: &Root) -> Result<()>;
    fn poll(&mut self, timeout: Duration) -> Vec<FsEvent>;  // globally ordered stream
}
```

**All three backends deliver a globally ordered event stream** — this property is what makes the cookie trick (§8) sound.

| FS op | inotify | fanotify (FID) | macOS FSEvents |
|---|---|---|---|
| create | `IN_CREATE` | `FAN_CREATE` + DFID_NAME | FileEvent created |
| delete | `IN_DELETE` / unpaired move | `FAN_DELETE` | removed |
| rename | cookie pair (degrade to Remove+Upsert on failure) | single `FAN_MOVED_FROM\|TO` | two events → remove+create |
| dir moved | parent's `MOVED_TO` only | same | same |
| content write | **ignore** | **ignore** | **ignore** |

- **fanotify FID** (`FAN_MARK_FILESYSTEM`, `FAN_REPORT_DFID_NAME`): whole-mount watch, zero per-dir kernel memory, atomic rename events. Map `(dev, file_handle) → dir id` via `name_to_handle_at` per dir during crawl. Unprivileged use needs recent kernels (~5.13+); verify on targets, else run system daemon as root.
- **inotify fallback**: one watch per dir (~1 KB kernel memory each; `max_user_watches` is real). `IN_Q_OVERFLOW` → `RescanSubtree(root, cause=overflow)`. `IN_IGNORED` → drop wd→id mapping (wds get reused). `IN_UNMOUNT` → drop subtree, wait for remount.
- **FSEvents**: file-event flag; persistent log + since-when ID gives free crash recovery (store last event ID in snapshot). `kFSEventsMustScanSubDirs` → rescan.
- **Ignore asymmetry**: inotify can decline to attach to excluded dirs (watch-time ignore); fanotify-FID and FSEvents cannot — they filter in the applier. Encode this in backend capabilities instead of special-casing.

Plumbing: backend thread → bounded channel (100k) → pending collection → applier. Never drop events silently: a full channel escalates to `RescanSubtree`, it does not discard.

---

## 7. Reconciliation (first-class subsystem)

This is the Watchman-inspired heart of v2. An overflow, a dropped watch, or a missed rename must degrade to *work*, never to silent desync.

**Pending collection.** One bounded structure owned by the applier:

1. **Sync cookies** (§8) — a distinct marker type, processed in stream order.
2. **FsEvents** — FIFO from the watcher; idempotent upsert/remove applies.
3. **Work items** — crawl tasks, `RescanSubtree` (coalesced per dir: re-enqueue on an already-pending dir is a no-op).

**Rescan = diff, not replace.** A rescan enumerates the dir/subtree and matches against existing index entries by `(parent_id, name)`:

- surviving entries **keep their IDs**; only genuine differences emit synthetic create/delete events into the pending collection (with fresh LSNs, so subscribers see them normally);
- the whole subtree is re-verified in one pass, so any number of missed events heals at once;
- if the rescan replaced the view of an entire root, bump generation → subscribers get `fresh: true` on next contact.

**Backoff and honesty.** Per-root rescan rate limit: min 1 s, exponential ×2, cap 5 min. Counters per root:

```
recrawl_count, last_rescan_cause ∈ {overflow, watch_dropped, cookie_timeout,
                                     journal_corrupt, mount_event, manual},
last_rescan_at, pending_items
```

**Degraded mode.** A root that can't be watched, or that fails N consecutive rescans, is marked `state = Degraded{reason}`. Queries keep answering from what's known; every response meta lists stale roots. Loudly stale beats silently wrong.

---

## 8. Sync (cookies)

The question every consumer eventually asks: *"has the daemon seen everything that happened before now?"* Answering with sleeps is garbage. Answer it the Watchman way:

**Mechanism.** `SyncOp{roots?}`:

1. For each watched root that is **writable** (probed, §4): create `.findex-cookie-<uuid>` in the root dir.
2. The cookie's create event arrives through the same globally ordered watcher stream as everything before it. The applier recognizes the name prefix and applies it as a marker (never indexed — Watchman's cookies famously leak into other tools' listings; we filter by prefix at apply time).
3. When the marker is applied **and** the pending collection's event/work queues for that scope are empty, the sync completes; the cookie file is unlinked (that unlink event is also filtered).
4. Timeout (default 2 s, one retry) → sync returns per-root status; a root that repeatedly fails cookie sync is flagged degraded-for-sync.

**Guarantee.** On success: every FS operation that completed before `sync` was called is reflected in the index. This holds because each backend's stream is globally ordered (cookie queued after prior ops) and the applier is single-threaded with FIFO applies.

**Read-only roots** (no cookie possible): fall back to draining the watcher fd + pending queue plus a short quiescence window. Weak guarantee; the response marks those roots `sync: "weak"` so callers know.

**Users:**

```bash
ff --sync main.rs              # deterministic: create file; ff; see it
ff --sync && ff --count ext:rs # scriptable settle point
```

The same op is what makes the differential test suite (§14) deterministic — no sleeps anywhere.

*Note on divergence from Watchman:* they prioritize cookies ahead of pending recrawl work because a completed recrawl publishes a fresh clock anyway. We require queues drained before sync returns, which is the stricter, simpler invariant; if syncs ever starve behind huge rescans, revisit with their argument (a pending rescan supersedes deltas, so an early cookie is safe for subscribers).

---

## 9. Persistence: snapshot + journal (and the journal is a clock)

```
snapshot.fnx:  magic "FNX2" | version | next_id | generation | roots[] | counts
               | names blob | entries array | last_lsn | checksum
journal.log:   [op u8][len u32][crc32][payload]   ADD / DEL / MOVE / FENCE / MARK
               LSN = monotonic record index
```

- **Load = mmap** (MAP_PRIVATE, < 50 ms at millions of entries); tombstone flips are COW. Replay journal tail on startup. Torn/corrupt tail: per-record CRC bounds the damage — keep serving, log it, enqueue `RescanSubtree(cause=journal_corrupt)`.
- **Cursors**: opaque `c:<gen>:<lsn>`. A cursor is valid only while `gen` matches; generation bumps (full-root rescan, compaction) invalidate all cursors by design — consumers resync from a full listing rather than apply deltas of uncertain provenance. This is Watchman's `is_fresh_instance` contract, expressed in our protocol.
- **Compaction**: build fresh snapshot off-thread, `arc_swap` it in, truncate journal, bump generation.
- Startup: load → replay → serve immediately (meta marks catching-up state) → background rescan only if the journal was unusable.

---

## 10. Query engine

Unchanged from v1, briefly: linear scan over the in-memory name arena — this is Everything's model and it's fast enough. Space = AND of substrings; `"..."` phrase; `*`/`?` wildcards; `ext:` `path:` `case:` `regex:` modifiers. ASCII case-folding on bytes; smart-case. One `aho-corasick` pass (SIMD) over all literal tokens, rarest-first with AND short-circuit; regex via the `regex` crate's literal prefilters. Path materialization only for surviving results, into a growable stack buffer. `--count` fast path skips materialization.

**Trigram posting lists (plocate-style) — v2, implemented.** `trigram.rs` builds, over the live non-root entries, a map from each **lowercased-ASCII name trigram** to a sorted posting list of entry ids. A query's *required* literals (positive `Substr` and `Ext` terms only — negation, glob, regex and `path:` terms carry no fixed substring over the name) are reduced to their trigram sets, and the candidate set is the intersection of those posting lists. The scan then visits only candidates.

The contract is that this is a **filter, never an authority**: every candidate still goes through the full `Query::matches` path, so the accelerator can change only the time taken, never the answer. Two consequences are load-bearing:

- Ids are stable across rescans (§3), so posting lists stay meaningful; the cache is keyed on `(generation, entry_count)`, so any addition or generation bump rebuilds it. A *deletion* leaves postings naming tombstoned ids — safe, because the accelerated path checks liveness and the matcher decides.
- `path:` terms are not accelerated (the index stores no path strings, §3), and literals shorter than three bytes give no trigram. Both fall through to the linear scan for their part of the query.

`crates/findex/tests/trigram_differential.rs` pins the contract against random trees and a corpus spanning every query shape; `crates/findex/examples/trigram_bench.rs` measures it. At 1 M entries the postings cost ~45 MiB against ~13 MiB of name bytes (a name averaging ~13 bytes yields ~11.6 trigrams), so the accelerator roughly quadruples name-side memory and queries run 6–60× faster — a good trade at 10 M+ files, which is why the design gates it at that scale. `path:`-heavy queries see no benefit.

New: every response's meta carries `{gen, lsn, fresh, elapsed_ms, stale_roots[]}` so clients can reason about consistency, not just results.

---

## 11. IPC protocol v2

NDJSON over unix socket. The op set is now cursor-native:

```
→ {"v":2,"op":"query","q":"src ext:rs","limit":50,"json":true}
← {"type":"meta","gen":7,"lsn":481221,"fresh":false,"elapsed_ms":7.1}
← {"type":"hit","path":"/home/me/proj/src/main.rs"}
← {"type":"done","count":412,"cursor":"c:7:481221"}

→ {"v":2,"op":"sync","timeout_ms":2000}
← {"type":"synced","roots":{"default":"ok","/mnt/ro":"weak"}}

→ {"v":2,"op":"since","cursor":"c:6:9001","q":""}
← {"type":"meta","gen":7,"fresh":true}          # gen mismatch → full listing
← {"type":"hit", ...}                            # else: delta events since LSN

→ {"v":2,"op":"subscribe","name":"gui","cursor":null}     # v1.5: change feed
→ {"v":2,"op":"stats"}   → {"recrawl_count":2,"last_rescan_cause":"overflow", ...}
→ {"v":2,"op":"rescan","root":"/data"}
```

Design rules carried from Watchman: coalesce per subscriber, slow clients catch up or resync, never let a client block the applier.

---

## 12. CLI

```bash
ff main.rs                       # instant search
ff -0 "test" | xargs -0 rg TODO  # pipe-friendly; exit 1 on no match
ff --count ext:pdf
ff --json --limit 1000 "2024" | fzf
ff --sync "notes"                # settle, then search — deterministic
ff --since c:7:481221            # what changed since
ff stats --roots                 # entries, backend, queue depth, drops,
                                 # recrawl_count, last_rescan_cause, per-root caps/state
ff rescan /data
ff update [--timeout 120] [ROOT...]  # reconcile all roots, then settle
ff watch                         # tail live events (debugging; preview of subscribe)
ff service install               # systemd user unit / launchd plist
```

`ff` auto-spawns the daemon if not running and streams crawl progress in the meta line. Results stream as found; default order = index order (fastest), `--sort` opt-in.

---

## 13. Concurrency model

Still boring, deliberately:

- Applier owns the pending collection and is the **single writer** behind a `RwLock`; queries take the read lock for the whole scan (~10–50 ms worst case). Writer stalls are absorbed by the bounded channel. Epoch/arc-swap schemes stay on the shelf until profiling demands them.
- Watcher, crawler/rescanner, compactor are separate threads.
- Sync ops wait on a oneshot registered per cookie; timeout is enforced by the applier's clock, so a stuck sync can't wedge a client thread.

---

## 14. Testing

The reconciliation layer is where this project lives or dies; test it like Watchman does:

1. **Fake watcher backend.** The `WatchBackend` trait has a test implementation that injects arbitrary `FsEvent` sequences without a real filesystem — including events that *lie* (drop a create, duplicate a rename, interleave). Model-based tests assert the index converges to truth after rescans. Runs in milliseconds, 100× faster than FS-backed tests.
2. **Real differential tests.** Random op sequences (create/delete/rename/move, via a temp dir) applied with `std::fs`; after each step: `ff --sync` (deterministic — no sleeps) → compare full index paths against `find`. This catches ~95% of watcher bugs.
3. **Fault injection**: simulated `IN_Q_OVERFLOW`; unpairable `MOVED_FROM`/`MOVED_TO`; `kill -9` mid-journal-write; mount/umount during crawl; watch-limit exhaustion (fill `max_user_watches` in a container).
4. **Backoff tests**: a pathological event storm must converge to bounded rescan rate — assert the backoff, don't discover it in production.
5. **24 h soak** with periodic `find` audits; zero drift is the bar.

---

## 15. Operations and hardening

Lessons from the Watchman/React-Native years, applied pre-emptively:

- **Socket**: `$XDG_RUNTIME_DIR/findex.sock` (0600); system instance `/run/findex.sock`. Never `/tmp` (stale-socket class of bugs).
- **State dir**: `$XDG_STATE_HOME/findex/<format-version>/` — versioned so a format change migrates or recrawls instead of corrupting. Stale-lock recovery via pidfile + `flock`.
- **Health in `ff stats`**: `recrawl_count`, `last_rescan_cause` (not just "a rescan happened"), queue depth, drops, per-root state/caps, last compaction. Every rescan logs its cause.
- **Backoff**: rate-limited rescans (§7); a pathological workload must not become a recrawl loop.
- **sysctl guidance** for the inotify fallback documented in `ff doctor`: `fs.inotify.max_user_watches`, `max_queued_events`.
- **Startup UX**: serve queries immediately from snapshot+journal; if the journal was unrecoverable, meta says `catching_up: true` until the rescan lands.

---

## 16. Pitfall checklist

- **Overlayfs** (Docker): `EXDEV` renames, unstable inodes → excluded by default, opt-in.
- **Case-insensitive mounts** (vfat/exfat/ntfs): store names as-is, per-root case flag from §4 probing.
- **Symlinks**: index the link, never follow; dangling links fine.
- **Editor tmp+rename dances**: coalesced by idempotent upserts; never debounce correctness-critical applies.
- **Permissions**: per-user daemon indexing what the user can read — no ACL filtering at all. Root "see everything" mode, if added later, stores `(mode, uid)` (+8 B/entry) or lazy-checks the parent chain at render.
- **Deep trees/long paths**: growable stack buffer for path materialization.
- **Cookie hygiene**: filter cookie names from index *and* from subscriber feeds.
- **Mount churn**: `RootGone` handling; optional scheduled rescans.

---

## 17. Targets and roadmap

| Metric | Target |
|---|---|
| Crawl throughput | ≥ 1 M files/s/core |
| Query p95 | < 20 ms @ 10 M files |
| Snapshot load | < 50 ms |
| Memory | ≈ 40 B/file |
| Sync (idle queue / busy) | p99 < 5 ms / < 200 ms, timeout-bounded |
| Convergence | zero drift over 24 h soak |

**Stack**: Rust — `libc` (getdents64/fanotify), `aho-corasick`, `memchr`, `crossbeam-channel`, `memmap2`, `arc-swap`, `regex`. (FSearch 0.2.x is the reference for the inotify path and GTK virtualization; Watchman docs for clocks/cookies/recrawl; Mercurial fsmonitor for the consumer side of `since`.)

- **v1**: inotify backend + full reconciliation core (pending collection, diff rescans, backoff, degraded mode) + cookie sync + snapshot/journal with cursors + linear scan + differential/fake-backend test harness. *Reconciliation and sync ship in v1 — v1's lesson from Watchman is that these are correctness, not polish.*
- **v1.5**: fanotify-FID backend, macOS FSEvents, `since`/`subscribe` ops live, regex/wildcards, `ff watch`, service units.
- **v2**: trigram accelerator (implemented — §10; filter-only, cached by generation), lazy attr sidecar keyed by stable IDs, GUI client on subscriptions.

The durable insight of this revision: Everything's *index* is easy to replicate; Watchman's *discipline* — one pending collection, diffs over replacements, cookies for settle, clocks over guesses, loud degradation — is what turns a toy into infrastructure. Build both in from the start.
