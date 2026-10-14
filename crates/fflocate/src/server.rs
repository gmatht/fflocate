//! Request handling shared by the daemon (§8, §11).
//!
//! The transport (unix socket, NDJSON) lives in the daemon crate; this module
//! holds the logic so it is testable without a socket, and so the CLI's
//! in-process fallback and the fake-backend model tests use exactly the same
//! code paths.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::applier::{now_ms, Shared};
use crate::health::Health;
use crate::proto::{Request, Response};
use crate::query::{self, Query};
use crate::roots::RootState;
use crate::sync::{self, SyncRegistry, SyncStatus};

/// Everything a handler needs.
pub struct ServerCtx {
    pub shared: Arc<Shared>,
    pub sync: Arc<SyncRegistry>,
    /// Backend name for stats/handshake.
    pub backend: String,
    /// A sender the daemon's applier thread listens on to create+register a
    /// cookie and to request a rescan. `None` in pure-read deployments.
    pub control: Option<ControlSender>,
    /// Who is asking, as established from `SO_PEERCRED` on the connection — never
    /// from anything the client says. `None` means "unknown caller", which in
    /// system mode means "show nothing" (see [`crate::ownership`]).
    ///
    /// In a per-user daemon this is `None` and no ownership filtering happens:
    /// the daemon already runs as the user.
    pub caller: Option<crate::ownership::Caller>,
}

/// A request from a client thread to the applier thread.
#[derive(Debug)]
pub enum Control {
    /// Ask the applier to create a cookie for root index `root` and register it
    /// with `token`;
    Cookie { token: u64, root: u32 },
    /// Ask the applier to rescan a root dir.
    RescanRoot { root: u32 },
}

pub type ControlSender = crossbeam_channel::Sender<Control>;

/// Result of handling one request: a list of response lines to write.
pub fn handle_request(ctx: &ServerCtx, req: &Request) -> Vec<Response> {
    let t0 = Instant::now();
    match req {
        Request::Hello => vec![Response::Pong {
            pid: std::process::id(),
            version: crate::proto::PROTOCOL_VERSION,
            backend: ctx.backend.clone(),
        }],
        Request::Query {
            q,
            limit,
            json: _,
            count,
            expression,
            glob,
            glob_includedotfiles,
            glob_noescape,
            suffix,
            path,
            relative_root,
            case_sensitive,
            fuzzy,
            max_distance,
            progress: _,
        } => {
            if expression.is_some() || glob.is_some() || suffix.is_some() || path.is_some() {
                let mut obj = serde_json::Map::new();
                if let Some(e) = expression {
                    obj.insert("expression".into(), e.clone());
                }
                if let Some(g) = glob {
                    obj.insert("glob".into(), g.clone());
                }
                if let Some(b) = glob_includedotfiles {
                    obj.insert("glob_includedotfiles".into(), serde_json::Value::Bool(*b));
                }
                if let Some(b) = glob_noescape {
                    obj.insert("glob_noescape".into(), serde_json::Value::Bool(*b));
                }
                if let Some(s) = suffix {
                    obj.insert("suffix".into(), s.clone());
                }
                if let Some(p) = path {
                    obj.insert("path".into(), p.clone());
                }
                if let Some(rr) = relative_root {
                    obj.insert(
                        "relative_root".into(),
                        serde_json::Value::String(rr.clone()),
                    );
                }
                if let Some(cs) = case_sensitive {
                    obj.insert("case_sensitive".into(), serde_json::Value::Bool(*cs));
                }
                handle_expr_query(ctx, &serde_json::Value::Object(obj), *limit, *count, t0)
            } else if *fuzzy {
                handle_fuzzy_query(ctx, q, *max_distance, *limit, *count, t0)
            } else {
                // On the plain-string path there is no expression to carry
                // `case_sensitive`, so it must be honoured here.
                handle_query(ctx, q, *limit, *count, *case_sensitive == Some(true), t0)
            }
        }
        Request::Sync { timeout_ms, roots } => handle_sync(ctx, *timeout_ms, roots.as_deref()),
        Request::Since { cursor, q } => handle_since(ctx, cursor, q),
        Request::Stats { roots } => handle_stats(ctx, *roots),
        Request::Rescan { root } => handle_rescan(ctx, root),
        Request::Subscribe { .. } => handle_subscribe(ctx),
    }
}

fn meta_preamble(ctx: &ServerCtx, fresh: bool, elapsed: Duration) -> Response {
    let ix = ctx.shared.index.read().unwrap();
    let roots = ctx.shared.roots.read().unwrap();
    let stale: Vec<String> = roots
        .iter()
        .filter(|r| !matches!(r.state, RootState::Ok))
        .map(|r| r.key())
        .collect();
    let catching_up = ctx.shared.health.read().unwrap().catching_up;
    // The daemon's own phase + crawl progress, read from the mirrored Health
    // (written on every applier tick — see `Daemon::tick`). Sent on every
    // Meta so a client can say *what* the daemon is doing, not merely *that*
    // it is behind. Crawl fields are only meaningful (and only sent) while
    // crawling; elsewhere they would be noise a client might misread.
    let (phase, crawl_entries, crawl_dirs_pending) = {
        let h = ctx.shared.health.read().unwrap();
        (
            h.phase.as_str().to_string(),
            h.crawl_entries,
            h.crawl_dirs_pending,
        )
    };
    let crawling = phase == crate::health::Phase::Crawling.as_str();
    // Lag = age of the oldest unapplied change. Aged here (not stored aged):
    // the arrival stamp is mirrored once per applier tick, so computing
    // `now - arrival` at reply time keeps the number honest between ticks.
    let lag_ms = {
        let oldest = ctx.shared.health.read().unwrap().oldest_queued_at_ms;
        if oldest == 0 {
            None
        } else {
            // Saturating: a clock step backwards must not underflow into a
            // colossal age (nor panic in debug).
            let now = now_ms();
            let age = now.saturating_sub(oldest);
            // A zero-millisecond age means "queued this very instant" — the
            // daemon is not measurably behind, so report nothing rather than
            // a cosmetic "0s behind".
            (age > 0).then_some(age)
        }
    };
    Response::Meta {
        gen: ctx.shared.generation(),
        lsn: ctx.shared.snapshot_lsn(),
        fresh,
        elapsed_ms: elapsed.as_secs_f64() * 1000.0,
        stale_roots: stale,
        catching_up: if catching_up { Some(true) } else { None },
        lag_ms,
        phase: Some(phase),
        crawl_entries: crawling.then_some(crawl_entries),
        crawl_dirs_pending: crawling.then_some(crawl_dirs_pending),
        backend: Some(ctx.backend.clone()),
        entries: Some(ix.live_count()),
    }
}

fn handle_query(
    ctx: &ServerCtx,
    raw: &str,
    limit: usize,
    count_only: bool,
    case_sensitive: bool,
    t0: Instant,
) -> Vec<Response> {
    let mut out = Vec::new();
    let _ = handle_query_streaming(
        ctx,
        raw,
        limit,
        count_only,
        case_sensitive,
        t0,
        &mut |r| {
            out.push(r);
            true
        },
        None,
        false,
    );
    out
}

/// Streaming query: `emit(Meta)` first (elapsed ≈ time-to-first-byte), then
/// one `emit(Hit)` per found hit as the scan finds it, then `emit(Done)` with
/// the exact total last. The hits and total are identical to [`handle_query`];
/// only the timing changes — a client rendering Hits as they arrive fills its
/// first page in milliseconds while the total is still being counted.
///
/// `emit` returns `false` to abort: the first refusal stops the scan and this
/// returns `false` without emitting `Done` (a partial total would lie). The
/// daemon's socket emit refuses once the client is gone, so a disconnected
/// client halts the scan instead of running it to a dead socket.
///
/// `emit` runs while the index read lock is held: write+flush promptly.
///
/// Public so the daemon transport can stream over the socket; the batched
/// [`handle_request`] (and its tests) delegate to this and are unchanged.
#[allow(clippy::too_many_arguments)]
pub fn handle_query_streaming(
    ctx: &ServerCtx,
    raw: &str,
    limit: usize,
    count_only: bool,
    case_sensitive: bool,
    t0: Instant,
    emit: &mut dyn FnMut(Response) -> bool,
    abort: query::ScanAbort<'_>,
    progress: bool,
) -> bool {
    let q = match Query::compile_with_case(raw, case_sensitive) {
        Ok(q) => q,
        Err(e) => {
            return emit(Response::Error { message: e });
        }
    };
    // Meta goes out BEFORE any scan guard is taken: this is what unblocks the
    // client's first paint. `elapsed_ms` here is time-to-first-byte; the final
    // cost lands in the client's own wall clock (and the GUI's `[Ns server]`
    // suffix).
    //
    // The ordering is load-bearing, not cosmetic. `meta_preamble` takes the
    // index read lock; the scan below holds it for its whole pass. Emitting
    // Meta while holding the scan guard nests a second read on the same
    // thread — normally granted, but fatal when the applier's write arrives
    // in between: std's write-preferring RwLock parks the nested read behind
    // the pending writer while the writer waits for the outer read, a
    // self-deadlock with zero CPU, silent stats, and a frozen journal
    // (proven live via gdb: main in write_contended, every serve thread in
    // read_contended, no holder, hello alive). Never nest index reads.
    if !emit(meta_preamble(ctx, false, t0.elapsed())) {
        return false;
    }
    // System mode cannot stream per-hit: `visible_chain` takes the index read
    // lock per id, which would nest exactly like Meta did. Collect-then-emit
    // instead (same TOCTOU as the old batched path, which filtered post-scan
    // too): Meta already went out early, page+total follow together.
    if let Some(caller) = &ctx.caller {
        // Batched-equivalent semantics: the old path materialized the first
        // `limit` matches (all of them under count_only) and counted visible
        // among those — never a full visible total. Mirror exactly.
        // `None` is a mid-collect abort (gone client): no `Done`, no page.
        let Some(collected) =
            collect_query_hits(ctx, &q, limit, count_only, abort, progress, t0, emit)
        else {
            return false;
        };
        let mut shown = 0u64;
        if !count_only {
            for (id, path) in &collected.hits {
                if !visible_chain(ctx, *id, caller) {
                    continue;
                }
                shown += 1;
                if !emit(Response::Hit {
                    path: bytes_to_string(path),
                    id: *id,
                }) {
                    return false;
                }
            }
        } else {
            shown = collected
                .hits
                .iter()
                .filter(|(id, _)| visible_chain(ctx, *id, caller))
                .count() as u64;
        }
        let cursor = ctx.shared.cursor().to_string_repr();
        return emit(Response::Done {
            count: shown,
            cursor,
        });
    }
    handle_query_streaming_inner(ctx, &q, limit, count_only, emit, abort, progress, t0)
}

/// Personal-mode scan core: single pass, per-hit emit straight to the caller,
/// exact total returned. Holds the index read guard for the pass — exactly
/// like the old batched scan — but performs NO nested lock acquisition while
/// holding it (see `handle_query_streaming`). `emit` may block on the client
/// socket; that extends the read hold (as any slow scan does) but cannot
/// self-deadlock: nothing else is acquired beneath it.
///
/// Returns `false` (without emitting `Done`) when the scan aborted mid-pass.
fn handle_query_streaming_inner(
    ctx: &ServerCtx,
    q: &Query,
    limit: usize,
    count_only: bool,
    emit: &mut dyn FnMut(Response) -> bool,
    abort: query::ScanAbort<'_>,
    progress: bool,
    t0: Instant,
) -> bool {
    // §17 v2 lock order: accelerator before index.
    #[cfg(feature = "trigram")]
    let trigram = Some(ctx.shared.trigram_index());
    #[cfg(feature = "trigram")]
    let trigram_guard = trigram.as_ref().map(|t| t.read().unwrap());
    let ix = ctx.shared.index.read().unwrap();
    let roots = ctx.shared.roots.read().unwrap();
    let mut root_paths: Vec<(u32, Vec<u8>)> = Vec::new();
    for r in roots.iter().filter(|r| r.is_local()) {
        let rid = crate::applier::root_dir_id(&ix, r);
        if rid != crate::index::ROOT_ID {
            let mut p = r.path.as_os_str().to_string_lossy().as_bytes().to_vec();
            root_paths.push((rid, std::mem::take(&mut p)));
        }
    }
    // Count-only: the scan counts matches; nothing is emitted but the
    // already-sent Meta + the Done below — Hits are swallowed, Progress
    // still flows (a count scan still takes time worth reporting). The
    // swallow arm never refuses (nothing to refuse): only a real socket
    // write can abort a count scan, via `Done` below.
    let mut last_progress: Option<Instant> = None;
    let mut forward = |ev: query::ScanEvent| -> bool {
        if count_only {
            if let query::ScanEvent::Hit(_) = ev {
                return true;
            }
        }
        forward_scan_event(emit, progress, t0, &mut last_progress, ev)
    };
    #[cfg(feature = "trigram")]
    let total = query::execute_with_trigram_emit(
        &ix,
        &root_paths,
        q,
        if count_only { 0 } else { limit },
        count_only,
        trigram_guard.as_deref(),
        &mut forward,
        abort,
        progress,
    );
    #[cfg(not(feature = "trigram"))]
    let total = query::execute_scan_emit(
        &ix,
        &root_paths,
        q,
        if count_only { 0 } else { limit },
        count_only,
        &mut forward,
        abort,
        progress,
    );
    // `total` counts matches exactly; under `count_only` the sink swallowed
    // all hits, so only Meta+Done go out. `None` is a mid-scan refusal: no
    // `Done`, no partial count — the client is gone, and the exact shape of
    // an unfinished reply is the transport's business (net.rs skips it).
    let Some(total) = total else { return false };
    let cursor = ctx.shared.cursor().to_string_repr();
    emit(Response::Done { count: total, cursor })
}

/// Collected (not streamed) hits for system mode: full materialization under
/// guards, filtered guard-free by the caller — the old batched semantics with
/// Meta already sent early. Returns all matching (id, path) pairs plus the
/// unfiltered total (the caller recounts visible).
struct CollectedHits {
    hits: Vec<(u32, Vec<u8>)>,
}

fn collect_query_hits(
    ctx: &ServerCtx,
    q: &Query,
    limit: usize,
    count_only: bool,
    abort: query::ScanAbort<'_>,
    progress: bool,
    t0: Instant,
    emit: &mut dyn FnMut(Response) -> bool,
) -> Option<CollectedHits> {
    #[cfg(feature = "trigram")]
    let trigram = Some(ctx.shared.trigram_index());
    #[cfg(feature = "trigram")]
    let trigram_guard = trigram.as_ref().map(|t| t.read().unwrap());
    let ix = ctx.shared.index.read().unwrap();
    let roots = ctx.shared.roots.read().unwrap();
    let mut root_paths: Vec<(u32, Vec<u8>)> = Vec::new();
    for r in roots.iter().filter(|r| r.is_local()) {
        let rid = crate::applier::root_dir_id(&ix, r);
        if rid != crate::index::ROOT_ID {
            let mut p = r.path.as_os_str().to_string_lossy().as_bytes().to_vec();
            root_paths.push((rid, std::mem::take(&mut p)));
        }
    }
    let mut hits: Vec<(u32, Vec<u8>)> = Vec::new();
    // Mirror the batched engine params: first `limit` matches materialized
    // (all of them under count_only), caller filters visible guard-free.
    let collect_limit = if count_only { 0 } else { limit };
    // One arm compiles (feature-gated); both propagate an abort the same
    // way: a mid-collect abort is a gone client, not a partial page.
    // Progress flows to the client during the collect (the page follows
    // after filtering); Hits accumulate for the guard-free filter below.
    let mut last_progress: Option<Instant> = None;
    let mut collect = |ev: query::ScanEvent| -> bool {
        match ev {
            query::ScanEvent::Hit(hit) => {
                hits.push((hit.id, hit.path));
                true
            }
            query::ScanEvent::Progress { .. } => {
                forward_scan_event(emit, progress, t0, &mut last_progress, ev)
            }
        }
    };
    #[cfg(feature = "trigram")]
    let collect_aborted = query::execute_with_trigram_emit(
        &ix,
        &root_paths,
        q,
        collect_limit,
        false,
        trigram_guard.as_deref(),
        &mut collect,
        abort,
        progress,
    )
    .is_none();
    #[cfg(not(feature = "trigram"))]
    let collect_aborted = query::execute_scan_emit(
        &ix,
        &root_paths,
        q,
        collect_limit,
        false,
        &mut collect,
        abort,
        progress,
    )
    .is_none();
    if collect_aborted {
        return None;
    }
    Some(CollectedHits { hits })
}

/// May the caller see this entry, *and* traverse every directory on the way to
/// it?
///
/// The second half is the part that is easy to get wrong: a world-readable file
/// inside a `0700` directory is not readable by anyone but the directory's owner,
/// yet the file's own mode says otherwise. So visibility is checked up the parent
/// chain, requiring traverse (`x`) on each directory and read (`r`) on the entry
/// itself.
///
/// Returns `true` when there is no caller set (a per-user daemon, which already
/// runs as that user) — this function is only reached in system mode.
fn visible_chain(ctx: &ServerCtx, id: u32, caller: &crate::ownership::Caller) -> bool {
    if caller.is_root() {
        return true;
    }
    let ix = ctx.shared.index.read().unwrap();
    let mut cur = id;
    loop {
        if cur == crate::index::ROOT_ID || cur as usize >= ix.entry_count() {
            // Ran off the top without reaching a root dir: unreachable.
            return false;
        }
        {
            let attrs = ctx.shared.ownership.read().unwrap();
            if !attrs.visible_to(cur, caller) {
                return false;
            }
        }
        let entry = match ix.entry(cur) {
            Some(e) => e,
            None => return false,
        };
        if entry.flags().is_root_dir() {
            return true;
        }
        cur = entry.parent;
    }
}

/// Default edit budget for `--fuzzy` when the client does not choose one.
/// 1 edit absorbs a typo (substitution, missing or extra character) without
/// turning the search into a different search.
pub const DEFAULT_FUZZY_DISTANCE: u32 = 1;

/// Fuzzy (approximate) query path (§10/§17 v2). Mirrors [`handle_query`] but
/// scores names by edit distance and returns them ranked by `(score, path)`.
///
/// Without the `trigram` cargo feature this is unavailable; rather than silently
/// returning exact results (which would look like "fuzzy found nothing"), it
/// reports an error — a caller that asked for fuzzy deserves to know it is not
/// there.
fn handle_fuzzy_query(
    ctx: &ServerCtx,
    raw: &str,
    max_distance: Option<u32>,
    limit: usize,
    count_only: bool,
    t0: Instant,
) -> Vec<Response> {
    let mut out = Vec::new();
    let _ = handle_fuzzy_streaming(
        ctx,
        raw,
        max_distance,
        limit,
        count_only,
        t0,
        &mut |r| {
            out.push(r);
            true
        },
        None,
        false,
    );
    out
}

/// Streaming fuzzy query. Meta goes out before the scan; hits follow after
/// ranking — fuzzy results are ordered by `(score, path)`, and ranking needs
/// every score, so per-hit streaming during the scan would emit the wrong
/// order. The win here is Meta-first (unblocks paint) plus flush-as-emitted;
/// first-page latency still waits on the scan, honestly.
///
/// Returns `false` (without emitting `Done`) on the first refused emit.
#[allow(clippy::too_many_arguments)]
pub fn handle_fuzzy_streaming(
    ctx: &ServerCtx,
    raw: &str,
    max_distance: Option<u32>,
    limit: usize,
    count_only: bool,
    t0: Instant,
    emit: &mut dyn FnMut(Response) -> bool,
    abort: query::ScanAbort<'_>,
    progress: bool,
) -> bool {
    #[cfg(not(feature = "trigram"))]
    {
        let _ = (ctx, raw, max_distance, limit, count_only, t0, abort, progress);
        return emit(Response::Error {
            message: "fuzzy search requires the `trigram` feature (not built in)".into(),
        });
    }

    #[cfg(feature = "trigram")]
    {
        let q = match Query::compile(raw) {
            Ok(q) => q,
            Err(e) => {
                return emit(Response::Error { message: e });
            }
        };
        let k = max_distance.unwrap_or(DEFAULT_FUZZY_DISTANCE);
        // Same lock-order rule as `handle_query`: build the accelerator before
        // taking the index read lock. The read guard lives for the query, as
        // there.
        let trigram = ctx.shared.trigram_index();
        let trigram_guard = trigram.read().unwrap();
        let ix = ctx.shared.index.read().unwrap();
        let roots = ctx.shared.roots.read().unwrap();
        let mut root_paths: Vec<(u32, Vec<u8>)> = Vec::new();
        // Foreign roots excluded, as in `handle_query` (§15).
        for r in roots.iter().filter(|r| r.is_local()) {
            let rid = crate::applier::root_dir_id(&ix, r);
            if rid != crate::index::ROOT_ID {
                root_paths.push((
                    rid,
                    r.path.as_os_str().to_string_lossy().as_bytes().to_vec(),
                ));
            }
        }
        // As in `handle_query`: never take the engine's count-only fast path when
        // we must filter, or the count would describe root's view, not the
        // caller's.
        let filtering = ctx.caller.is_some();
        let engine_count_only = count_only && !filtering;
        let effective_limit = if count_only { 0 } else { limit };
        // Abortable: fuzzy scores the whole candidate set before the first
        // emit, so the poll must live in compute, not just in the emit tail.
        // `None` is a mid-compute abort: no ranking, no `Done`.
        // Progress during scoring: fuzzy ranks the whole candidate set
        // before the first Hit, so without this the percent would sit at
        // zero for the entire compute. Same mapper, same gate, same guard
        // rules (socket-write only, under the scoring guards).
        let mut last_progress: Option<Instant> = None;
        let mut progress_emit = |ev: query::ScanEvent| -> bool {
            match ev {
                query::ScanEvent::Hit(_) => true, // scoring emits no Hits
                query::ScanEvent::Progress { .. } => {
                    forward_scan_event(emit, progress, t0, &mut last_progress, ev)
                }
            }
        };
        let Some(res) = query::execute_fuzzy_abortable(
            &ix,
            &root_paths,
            &q,
            k,
            effective_limit,
            engine_count_only,
            Some(&trigram_guard),
            abort,
            &mut progress_emit,
            progress,
        ) else {
            return false;
        };
        // Meta first (unblocks paint) — but AFTER dropping the scan
        // guards: `meta_preamble` takes the index read lock, and nesting it
        // under the held scan guard self-deadlocks against a pending applier
        // write (std RwLock is write-preferring). Same for the ownership
        // filter below (`visible_chain` reads the index per id) and the
        // trigram guard (postings-read held across an index-read inverts the
        // applier's index-then-postings order). Drop everything first.
        let cursor = ctx.shared.cursor().to_string_repr();
        drop(roots);
        drop(ix);
        drop(trigram_guard);
        if !emit(meta_preamble(ctx, false, t0.elapsed())) {
            return false;
        }

        // The same ownership filter as the exact path. Fuzzy results are still
        // results: skipping this here would leak every file the caller cannot read
        // (and it did, before this line existed).
        emit_filtered_results(ctx, res, cursor, count_only, emit)
    }
}

/// Shared tail for handlers whose engine returns `Results` wholesale (fuzzy:
/// ranking needs the full set): emit visible hits in order, then exact Done.
/// Runs guard-free (see `handle_fuzzy_streaming`): the per-hit ownership
/// filter reads the index, which must never nest under a scan guard.
///
/// Returns `false` (without emitting `Done`) on the first refused emit.
fn emit_filtered_results(
    ctx: &ServerCtx,
    res: query::Results,
    cursor: String,
    count_only: bool,
    emit: &mut dyn FnMut(Response) -> bool,
) -> bool {
    if count_only {
        let total = match &ctx.caller {
            Some(caller) => res
                .hits
                .iter()
                .filter(|h| visible_chain(ctx, h.id, caller))
                .count() as u64,
            None => res.total,
        };
        return emit(Response::Done { count: total, cursor });
    }
    let mut shown = 0u64;
    for h in &res.hits {
        if let Some(caller) = &ctx.caller {
            if !visible_chain(ctx, h.id, caller) {
                continue;
            }
        }
        shown += 1;
        if !emit(Response::Hit {
            path: bytes_to_string(&h.path),
            id: h.id,
        }) {
            return false;
        }
    }
    let total = if ctx.caller.is_some() { shown } else { res.total };
    emit(Response::Done { count: total, cursor })
}

/// The expression-tree query path (§10). Parses the Watchman-style query object
/// and evaluates it over the index, honouring `relative_root`.
fn handle_expr_query(
    ctx: &ServerCtx,
    query: &serde_json::Value,
    limit: usize,
    count_only: bool,
    t0: Instant,
) -> Vec<Response> {
    let mut out = Vec::new();
    let _ = handle_expr_streaming(
        ctx,
        query,
        limit,
        count_only,
        t0,
        &mut |r| {
            out.push(r);
            true
        },
        None,
        false,
    );
    out
}

/// Streaming expression query: same Meta-first/Hits-as-found/Done-last shape
/// as [`handle_query_streaming`], driven by `execute_expr_emit`. Returns
/// `false` (without emitting `Done`) on the first refused emit.
pub fn handle_expr_streaming(
    ctx: &ServerCtx,
    query: &serde_json::Value,
    limit: usize,
    count_only: bool,
    t0: Instant,
    emit: &mut dyn FnMut(Response) -> bool,
    abort: query::ScanAbort<'_>,
    progress: bool,
) -> bool {
    let expr = match crate::expr::parse_query(query) {
        Ok(e) => e,
        Err(e) => {
            return emit(Response::Error { message: e });
        }
    };
    let rel_root = query
        .get("relative_root")
        .and_then(|v| v.as_str())
        .map(|s| s.as_bytes().to_vec())
        .unwrap_or_default();

    // Root table in its own scope: acquired AND released before Meta.
    // Meta takes the index read lock itself; acquiring the scan guards first
    // and emitting Meta under them nests a second read on this thread — the
    // exact self-deadlock the plain path documents (it wedged deterministically
    // under churn: this expr path held the guards first with a stale comment
    // claiming otherwise). Scope disciplines it: no guard outlives this block.
    let root_paths: Vec<(u32, Vec<u8>)> = {
        let ix = ctx.shared.index.read().unwrap();
        let roots = ctx.shared.roots.read().unwrap();
        let mut root_paths: Vec<(u32, Vec<u8>)> = Vec::new();
        // Foreign roots excluded, as in `handle_query` (§15).
        for r in roots.iter().filter(|r| r.is_local()) {
            let rid = crate::applier::root_dir_id(&ix, r);
            if rid != crate::index::ROOT_ID {
                root_paths.push((
                    rid,
                    r.path.as_os_str().to_string_lossy().as_bytes().to_vec(),
                ));
            }
        }
        root_paths
    };
    // Meta with NO guards held (never nest index reads — see plain path).
    if !emit(meta_preamble(ctx, false, t0.elapsed())) {
        return false;
    }
    // System mode: collect under guards, filter guard-free (same TOCTOU as
    // the batched path). Personal mode streams per-hit below.
    if let Some(caller) = &ctx.caller {
        // Batched-equivalent params: first `limit` (all under count_only).
        // Scan guards acquired here, AFTER Meta (never nest).
        let collect_limit = if count_only { 0 } else { limit };
        let mut hits: Vec<(u32, Vec<u8>)> = Vec::new();
        {
            let ix = ctx.shared.index.read().unwrap();
            let mut last_progress: Option<Instant> = None;
            let mut collect = |ev: query::ScanEvent| -> bool {
                match ev {
                    query::ScanEvent::Hit(hit) => {
                        hits.push((hit.id, hit.path));
                        true
                    }
                    query::ScanEvent::Progress { .. } => {
                        forward_scan_event(emit, progress, t0, &mut last_progress, ev)
                    }
                }
            };
            let collect_aborted = query::execute_expr_emit(
                &ix,
                &root_paths,
                &rel_root,
                &expr,
                collect_limit,
                false,
                &mut collect,
                abort,
                progress,
            )
            .is_none();
            if collect_aborted {
                return false;
            }
        }
        let mut shown = 0u64;
        if !count_only {
            for (id, path) in &hits {
                if !visible_chain(ctx, *id, caller) {
                    continue;
                }
                shown += 1;
                if !emit(Response::Hit {
                    path: bytes_to_string(path),
                    id: *id,
                }) {
                    return false;
                }
            }
        } else {
            shown = hits
                .iter()
                .filter(|(id, _)| visible_chain(ctx, *id, caller))
                .count() as u64;
        }
        let cursor = ctx.shared.cursor().to_string_repr();
        return emit(Response::Done {
            count: shown,
            cursor,
        });
    }
    // Count-only swallows Hits but still reports Progress (the scan still
    // takes time worth showing). Same mapper as the plain path.
    let mut last_progress: Option<Instant> = None;
    let mut forward = |ev: query::ScanEvent| -> bool {
        if count_only {
            if let query::ScanEvent::Hit(_) = ev {
                return true;
            }
        }
        forward_scan_event(emit, progress, t0, &mut last_progress, ev)
    };
    // Scan guards acquired here, AFTER Meta (never nest).
    let total = {
        let ix = ctx.shared.index.read().unwrap();
        query::execute_expr_emit(
            &ix,
            &root_paths,
            &rel_root,
            &expr,
            if count_only { 0 } else { limit },
            count_only,
            &mut forward,
            abort,
            progress,
        )
    };
    // `None` is a mid-scan refusal: no `Done`, no partial count.
    let Some(total) = total else { return false };
    let cursor = ctx.shared.cursor().to_string_repr();
    emit(Response::Done { count: total, cursor })
}

/// The sync engine (§8). For each writable root we create a cookie, ask the
/// applier to register it, then wait for completion up to `timeout_ms`.
///
/// Read-only roots fall back to a weak guarantee: they are reported `"weak"`.
fn handle_sync(ctx: &ServerCtx, timeout_ms: u64, only_roots: Option<&[String]>) -> Vec<Response> {
    let timeout = Duration::from_millis(timeout_ms.max(1));
    let roots_snapshot: Vec<(usize, u32, String, std::path::PathBuf, bool)> = {
        let ix = ctx.shared.index.read().unwrap();
        let roots = ctx.shared.roots.read().unwrap();
        roots
            .iter()
            .enumerate()
            .filter(|(_, r)| match only_roots {
                Some(list) => list.iter().any(|k| k == &r.key()),
                None => true,
            })
            // Foreign roots (dual-boot shared store) are reported below,
            // never synced: their paths do not resolve here (§15).
            .filter(|(_, r)| r.is_local())
            .map(|(i, r)| {
                let rid = crate::applier::root_dir_id(&ix, r);
                (i, rid, r.key(), r.path.clone(), r.caps.writable)
            })
            .collect()
    };

    let mut out_map = std::collections::BTreeMap::new();
    // Roots excluded above keep their voice: report them as foreign rather
    // than silently dropping them from the map.
    for r in ctx.shared.roots.read().unwrap().iter() {
        let selected = match &only_roots {
            Some(list) => list.iter().any(|k| k == &r.key()),
            None => true,
        };
        if selected && !r.is_local() {
            out_map.insert(r.key(), SyncStatus::Foreign.as_str().to_string());
        }
    }
    // Waiters that must be serviced concurrently so one slow root cannot block
    // the others' cookies.
    let mut waits: Vec<(String, crossbeam_channel::Receiver<()>)> = Vec::new();
    let mut to_retry: Vec<(String, String)> = Vec::new();

    let mut next_token_base = now_ms() ^ (std::process::id() as u64) << 20;

    for (_i, rid, key, path, writable) in &roots_snapshot {
        if !*writable {
            out_map.insert(key.clone(), SyncStatus::Weak.as_str().to_string());
            continue;
        }
        let token = {
            next_token_base = next_token_base.wrapping_add(1);
            next_token_base
        };
        let name = sync::cookie_name(token);
        let cookie_path = match sync::create_cookie(path, token) {
            Ok(p) => p,
            Err(_) => {
                // Could not create the cookie (permissions, race): weak.
                out_map.insert(key.clone(), SyncStatus::Weak.as_str().to_string());
                continue;
            }
        };
        let rx = ctx
            .sync
            .expect(token, *rid, name.clone(), Some(cookie_path.clone()));
        // Ask the applier to register the cookie with the pending collection.
        let mut asked = false;
        if let Some(ctl) = &ctx.control {
            asked = ctl.send(Control::Cookie { token, root: *rid }).is_ok();
        }
        if !asked {
            // No applier control channel (read-only server): fall back to
            // draining and a short quiescence window — the weak guarantee.
            let _ = ctx.sync.forget(token);
            let _ = std::fs::remove_file(&cookie_path);
            out_map.insert(key.clone(), SyncStatus::Weak.as_str().to_string());
            continue;
        }
        waits.push((key.clone(), rx));
        to_retry.push((key.clone(), format!("{token}")));
    }

    // Wait phase.
    let deadline = Instant::now() + timeout;
    for (key, rx) in &waits {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || !sync::wait_for(rx, remaining) {
            out_map.insert(key.clone(), SyncStatus::Timeout.as_str().to_string());
        } else {
            out_map.insert(key.clone(), SyncStatus::Ok.as_str().to_string());
        }
    }
    // Fallback for timeouts: clean up the cookie file, then mark weak rather
    // than lying about success.
    for (key, tok) in &to_retry {
        if out_map
            .get(key)
            .map(|s| s == SyncStatus::Timeout.as_str())
            .unwrap_or(false)
        {
            if let Ok(token) = tok.parse::<u64>() {
                if let Some(p) = ctx.sync.forget(token) {
                    let _ = std::fs::remove_file(p);
                }
            }
            out_map.insert(key.clone(), SyncStatus::Weak.as_str().to_string());
        }
    }

    vec![Response::Synced { roots: out_map }]
}

fn handle_since(ctx: &ServerCtx, cursor: &str, q: &str) -> Vec<Response> {
    let current_gen = ctx.shared.generation();
    let parsed = match crate::cursor::Cursor::parse_with_gen(cursor, current_gen) {
        Some(c) => c,
        None => {
            return vec![Response::Error {
                message: format!("bad cursor: {cursor}"),
            }]
        }
    };
    let stale = parsed.is_stale(current_gen);
    let t0 = Instant::now();
    let mut out = vec![meta_preamble(ctx, stale, t0.elapsed())];
    if stale {
        // Generation mismatch → a full listing stands in for the delta (§9).
        if q.is_empty() {
            return out;
        }
    }
    // Delta: replay the change ring since the cursor, and (when a query is
    // given) filter the current index for matching paths.
    let changes = ctx.shared.changes.read().unwrap().since(parsed);
    let ix = ctx.shared.index.read().unwrap();
    let roots = ctx.shared.roots.read().unwrap();
    let root_paths: Vec<(u32, Vec<u8>)> = roots
        .iter()
        .filter_map(|r| {
            let rid = crate::applier::root_dir_id(&ix, r);
            (rid != crate::index::ROOT_ID).then(|| {
                (
                    rid,
                    r.path.as_os_str().to_string_lossy().as_bytes().to_vec(),
                )
            })
        })
        .collect();
    let mut buf = Vec::new();
    for c in changes {
        // Emit one event per matching path at/after that LSN is not tracked per
        // path in v1, so we emit a coarse "flush" event carrying the cursor.
        out.push(Response::Event {
            cursor: c.to_string_repr(),
            action: "flush".to_string(),
            path: String::new(),
        });
    }
    if !q.is_empty() {
        if let Ok(query) = Query::compile(q) {
            let res = query::execute(&ix, &root_paths, &query, 0, false);
            for h in &res.hits {
                if ix.materialize_into(h.id, b"/", &mut buf).is_some() {
                    out.push(Response::Event {
                        cursor: ctx.shared.cursor().to_string_repr(),
                        action: "hit".to_string(),
                        path: bytes_to_string(&h.path),
                    });
                }
            }
        }
    }
    // A `since` reply is bounded: it always ends with a `done` carrying the
    // current cursor, so the client (and any subscriber) knows it caught up.
    out.push(Response::Done {
        count: out.len() as u64,
        cursor: ctx.shared.cursor().to_string_repr(),
    });
    out
}

fn handle_stats(ctx: &ServerCtx, per_root: bool) -> Vec<Response> {
    let h: Health = ctx.shared.health.read().unwrap().clone();
    let ix = ctx.shared.index.read().unwrap();
    let roots = ctx.shared.roots.read().unwrap();
    let mut stats = h.as_json();
    let obj = stats.as_object_mut().unwrap();
    obj.insert("gen".into(), serde_json::json!(ctx.shared.generation()));
    obj.insert("lsn".into(), serde_json::json!(ctx.shared.snapshot_lsn()));
    obj.insert("entries".into(), serde_json::json!(ix.live_count()));
    obj.insert("tombstones".into(), serde_json::json!(ix.tombstone_count()));
    obj.insert("backend".into(), serde_json::json!(ctx.backend));
    // Why the backend is what it is (§4: a mis-probed choice should be visible,
    // not mysterious). Only meaningful on Linux; elsewhere it says so.
    obj.insert(
        "fanotify".into(),
        serde_json::json!({
            "available": crate::mount::fanotify_available(),
            "note": crate::mount::fanotify_availability_note(),
        }),
    );
    if per_root {
        let roots_json: Vec<serde_json::Value> = roots
            .iter()
            .map(|r| {
                serde_json::json!({
                    "path": r.key(),
                    "enabled": r.enabled,
                    "owner": r.owner_os,
                    "local": r.is_local(),
                    "drive": r.drive.as_ref().map(|d| serde_json::json!({
                        "disk_serial": d.disk_serial,
                        "volume_uuid": d.volume_uuid,
                    })).unwrap_or(serde_json::Value::Null),
                    "state": r.state.label(),
                    "recrawl_count": r.recrawl_count,
                    "last_rescan_cause": r.last_rescan_cause.map(|c| c.as_str()),
                    "pending_items": r.pending_items,
                    "caps": {
                        "file_handles": r.caps.file_handles,
                        "dtype_unreliable": r.caps.dtype_unreliable,
                        "case_insensitive": r.caps.case_insensitive,
                        "writable": r.caps.writable,
                        "watch_time_ignore": r.caps.watch_time_ignore,
                        "fstype": r.caps.fstype,
                    },
                })
            })
            .collect();
        obj.insert("roots".into(), serde_json::json!(roots_json));
    }
    vec![Response::Stats { stats }]
}

fn handle_rescan(ctx: &ServerCtx, root: &str) -> Vec<Response> {
    // Keys are separator-canonical; user input may not be (`C:\tree`).
    let root = crate::util::normalize_separators(std::path::Path::new(root))
        .to_string_lossy()
        .into_owned();
    let (found, rid) = {
        let ix = ctx.shared.index.read().unwrap();
        let roots = ctx.shared.roots.read().unwrap();
        match roots.iter().find(|r| r.key() == root) {
            // Rescanning a foreign root would reconcile a subtree this OS
            // cannot see and risk dropping it as vanished: refuse loudly (§15).
            Some(r) if !r.is_local() => {
                return vec![Response::Error {
                    message: format!("foreign root (recorded by another OS): {root}"),
                }]
            }
            Some(r) => (true, crate::applier::root_dir_id(&ix, r)),
            None => (false, crate::index::ROOT_ID),
        }
    };
    if !found {
        return vec![Response::Error {
            message: format!("unknown root: {root}"),
        }];
    }
    if let Some(ctl) = &ctx.control {
        let _ = ctl.send(Control::RescanRoot { root: rid });
    }
    vec![Response::Rescanning {
        root: root.to_string(),
    }]
}

fn handle_subscribe(ctx: &ServerCtx) -> Vec<Response> {
    // v1.5 preview: emit meta then the current tail of the change log. A real
    // subscriber would be fed by the applier; the protocol shape is final.
    let t0 = Instant::now();
    let mut out = vec![meta_preamble(ctx, false, t0.elapsed())];
    out.push(Response::Done {
        count: 0,
        cursor: ctx.shared.cursor().to_string_repr(),
    });
    out
}

pub fn bytes_to_string(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Minimum wall time between two `Progress` records on one reply. The engines
/// fire progress per 4096-entry chunk (~4 ms of scan); a 2.2M-entry scan would
/// otherwise emit ~540 Progress lines. Gated to ~10/s, fast queries send 0–2
/// (the initial `(0, total)` plus maybe one) — the GUI's percent simply never
/// appears for them.
const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(100);

/// Map one engine event to its response through `emit`, rate-gating `Progress`.
///
/// Hits always forward. `Progress` forwards when opted in (`want`) AND the
/// gate allows: the initial `(0, total)` (denominator announcement) plus at
/// most one per `PROGRESS_MIN_INTERVAL`. Gated-out reports return `true`
/// (skipped, not refused — skipping is not an abort). A refused *forwarded*
/// record returns `false` and the engine stops with no `Done`.
///
/// Captures nothing but `emit` and timestamps: no locks, so it is exactly as
/// safe under the scan's index read guard as a Hit — review must keep it
/// that way (see the `streaming_lockorder` saga).
fn forward_scan_event(
    emit: &mut dyn FnMut(Response) -> bool,
    want: bool,
    t0: Instant,
    last_progress: &mut Option<Instant>,
    ev: query::ScanEvent,
) -> bool {
    match ev {
        query::ScanEvent::Hit(hit) => emit(Response::Hit {
            path: bytes_to_string(&hit.path),
            id: hit.id,
        }),
        query::ScanEvent::Progress { scanned, total } => {
            if !want {
                return true;
            }
            // Rate gate, anchored on the *scan start* rather than on the last
            // send, and never suppressing a completed scan.
            //
            // Why not "100 ms since the last record": that form emits the
            // `(0, total)` announcement, then nothing at all until 100 ms has
            // elapsed *since that send*. A scan finishing inside the window
            // therefore reported only "0 of N" — the percentage sat at 0% and
            // never moved, which is exactly the bug this gate caused on a real
            // index (a `"comp"` query finishes in ~30 ms).
            //
            // Two changes fix it and neither weakens the gate's purpose
            // (bounding records on a *long* scan):
            //   * the first record after `PROGRESS_MIN_INTERVAL` of elapsed
            //     scan time goes out, so a short scan reports its real
            //     position rather than only its start;
            //   * a finished pass (`scanned == total`) always goes out, so the
            //     client sees the scan complete even on a fast query.
            let done = total > 0 && scanned >= total;
            let due = last_progress
                .map(|t| t.elapsed() >= PROGRESS_MIN_INTERVAL)
                .unwrap_or(true);
            if !done && !due {
                return true;
            }
            *last_progress = Some(Instant::now());
            let _ = t0;
            emit(Response::Progress { scanned, total })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::applier::Shared;
    use crate::index::{EntryFlags, Index, ROOT_ID};
    use crate::persist::Store;
    use crate::roots::Root;
    use std::path::PathBuf;

    fn ctx_with_tree() -> (ServerCtx, tempfile::TempDir, Arc<Shared>) {
        let d = tempfile::tempdir().unwrap();
        // Real tree under the tempdir.
        std::fs::create_dir_all(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/main.rs"), b"fn main(){}").unwrap();
        std::fs::write(d.path().join("README.md"), b"readme").unwrap();

        let mut ix = Index::new();
        let basename = d.path().file_name().unwrap().to_string_lossy().to_string();
        let rid = ix.insert(
            ROOT_ID,
            basename.as_bytes(),
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        let src = ix.insert(rid, b"src", EntryFlags::EMPTY.with_dir(true));
        ix.insert(src, b"main.rs", EntryFlags::EMPTY);
        ix.insert(rid, b"README.md", EntryFlags::EMPTY);
        let mut root = Root::new(0, d.path().to_path_buf());
        root.caps.writable = true;
        let shared = Shared::new(ix, vec![root]);
        let ctx = ServerCtx {
            shared: shared.clone(),
            sync: SyncRegistry::new(),
            backend: "fake".into(),
            control: None,
            caller: None,
        };
        let store_dir = d.path().join("state");
        let _ = Store::open(&store_dir);
        (ctx, d, shared)
    }

    /// Two roots in one index: a local one (this OS or legacy) and a
    /// foreign one (recorded by a fictitious other OS). Hits must come only
    /// from the local side.
    fn ctx_with_foreign_root() -> (ServerCtx, tempfile::TempDir, Arc<Shared>) {
        let d = tempfile::tempdir().unwrap();
        let mut ix = Index::new();
        let mine = ix.insert(
            ROOT_ID,
            b"mine",
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        ix.insert(mine, b"local.txt", EntryFlags::EMPTY);
        let theirs = ix.insert(
            ROOT_ID,
            b"theirs",
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        ix.insert(theirs, b"remote.txt", EntryFlags::EMPTY);
        let mut local = Root::new(0, PathBuf::from("/mnt/mine"));
        local.caps.writable = true;
        let mut foreign = Root::new(1, PathBuf::from("/mnt/theirs"));
        foreign.caps.writable = true;
        foreign.owner_os = Some("definitely-not-this-os".into());
        assert!(!foreign.is_local(), "fixture must be foreign here");
        let shared = Shared::new(ix, vec![local, foreign]);
        let ctx = ServerCtx {
            shared: shared.clone(),
            sync: SyncRegistry::new(),
            backend: "fake".into(),
            control: None,
            caller: None,
        };
        (ctx, d, shared)
    }

    fn hits_of(out: &[Response]) -> Vec<String> {
        out.iter()
            .filter_map(|r| match r {
                Response::Hit { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn foreign_roots_are_excluded_from_queries() {
        let (ctx, _d, _s) = ctx_with_foreign_root();
        let out = handle_request(&ctx, &Request::query("txt", 0, true, false));
        assert_eq!(hits_of(&out), vec!["/mnt/mine/local.txt".to_string()]);
        let out = handle_request(
            &ctx,
            &Request::Query {
                q: String::new(),
                limit: 0,
                json: false,
                count: false,
                expression: Some(serde_json::json!(["suffix", "txt"])),
                glob: None,
                glob_includedotfiles: None,
                glob_noescape: None,
                suffix: None,
                path: None,
                relative_root: None,
                case_sensitive: None,
                fuzzy: false,
                max_distance: None,
            progress: false,
            },
        );
        assert_eq!(hits_of(&out), vec!["/mnt/mine/local.txt".to_string()]);
    }

    #[test]
    fn foreign_roots_report_foreign_and_refuse_rescan() {
        let (ctx, _d, _s) = ctx_with_foreign_root();
        let out = handle_request(
            &ctx,
            &Request::Sync {
                timeout_ms: 50,
                roots: None,
            },
        );
        match out.last() {
            Some(Response::Synced { roots }) => {
                // Local root goes weak (no control channel in this harness);
                // the foreign root must say `foreign`, never attempted.
                assert_eq!(
                    roots.get("/mnt/theirs").map(String::as_str),
                    Some("foreign")
                );
                assert!(
                    roots.contains_key("/mnt/mine"),
                    "local roots still reported"
                );
            }
            other => panic!("expected synced, got {other:?}"),
        }
        let out = handle_request(
            &ctx,
            &Request::Rescan {
                root: "/mnt/theirs".into(),
            },
        );
        match out.first() {
            Some(Response::Error { message }) => assert!(message.contains("foreign"), "{message}"),
            other => panic!("rescan of a foreign root must refuse, got {other:?}"),
        }
        let out = handle_request(&ctx, &Request::Stats { roots: true });
        match out.first() {
            Some(Response::Stats { stats }) => {
                let roots = stats.get("roots").and_then(|v| v.as_array()).unwrap();
                let theirs = roots
                    .iter()
                    .find(|r| r.get("path").and_then(|v| v.as_str()) == Some("/mnt/theirs"))
                    .unwrap();
                assert_eq!(theirs.get("local").and_then(|v| v.as_bool()), Some(false));
                assert_eq!(
                    theirs.get("owner").and_then(|v| v.as_str()),
                    Some("definitely-not-this-os")
                );
            }
            other => panic!("expected stats, got {other:?}"),
        }
    }

    #[test]
    fn meta_reports_lag_age_when_work_is_queued() {
        // End-to-end for the lag path: a queued-but-unapplied change makes
        // Meta carry its age; a drained daemon omits the field entirely.
        let (ctx, _d, shared) = ctx_with_tree();
        let meta_lag = |ctx: &ServerCtx| {
            match handle_request(ctx, &Request::query("rs", 0, false, false)).first() {
                Some(Response::Meta { lag_ms, .. }) => *lag_ms,
                other => panic!("expected meta, got {other:?}"),
            }
        };
        // Caught up: no lag field.
        assert_eq!(meta_lag(&ctx), None, "drained daemon reports no lag");

        // Queue work and stamp it ~120ms in the past: the age must surface,
        // and must be measured at reply time (not frozen at the tick).
        {
            let mut h = shared.health.write().unwrap();
            h.oldest_queued_at_ms = crate::applier::now_ms() - 120;
        }
        let lag = meta_lag(&ctx).expect("queued work must report lag");
        assert!(
            (100..1000).contains(&lag),
            "lag should be the ~120 ms age, got {lag}"
        );

        // A clock step backwards must not underflow into a colossal age.
        {
            let mut h = shared.health.write().unwrap();
            h.oldest_queued_at_ms = crate::applier::now_ms() + 10_000;
        }
        assert_eq!(
            meta_lag(&ctx),
            None,
            "a future stamp saturates to 'no measurable lag', never a huge number"
        );

        // The sentinel 0 means "nothing queued" (the daemon's caught-up state).
        {
            let mut h = shared.health.write().unwrap();
            h.oldest_queued_at_ms = 0;
        }
        assert_eq!(meta_lag(&ctx), None);
    }

    #[test]
    fn query_returns_meta_hits_done() {
        let (ctx, _d, _s) = ctx_with_tree();
        let out = handle_request(&ctx, &Request::query("rs", 0, true, false));
        assert!(matches!(out.first(), Some(Response::Meta { .. })));
        assert!(matches!(out.last(), Some(Response::Done { .. })));
        let hits: Vec<_> = out
            .iter()
            .filter_map(|r| match r {
                Response::Hit { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].ends_with("/src/main.rs"));
    }

    #[test]
    fn count_fast_path() {
        let (ctx, _d, _s) = ctx_with_tree();
        let out = handle_request(&ctx, &Request::query("rs", 0, false, true));
        let done = out
            .iter()
            .find_map(|r| match r {
                Response::Done { count, .. } => Some(*count),
                _ => None,
            })
            .unwrap();
        assert_eq!(done, 1);
        assert!(!out.iter().any(|r| matches!(r, Response::Hit { .. })));
    }

    #[test]
    fn streaming_abort_skips_done_and_stops_the_scan() {
        // A refused emit aborts the reply: `false` out, no `Done` after it
        // (a partial count would lie), and only the records before the
        // refusal were delivered. This is the server half of the daemon's
        // dead-client halt — net.rs refuses once the socket is gone.
        let (ctx, _d, _s) = ctx_with_tree();
        let mut out = Vec::new();
        let mut hits_seen = 0usize;
        let ok = handle_query_streaming(
            &ctx,
            "rs",
            0,
            false,
            false,
            std::time::Instant::now(),
            &mut |r| {
                if matches!(r, Response::Hit { .. }) {
                    hits_seen += 1;
                    // The tree has exactly one "rs" hit (src/main.rs):
                    // refuse it, so the scan aborts on its only hit.
                    return false;
                }
                out.push(r);
                true
            },
            None,
            false,
        );
        assert!(!ok, "a refused Hit must abort the streaming reply");
        assert_eq!(hits_seen, 1);
        assert!(
            matches!(out.first(), Some(Response::Meta { .. })),
            "Meta went out before the abort, got {out:?}"
        );
        assert!(
            !out.iter().any(|r| matches!(r, Response::Done { .. })),
            "no Done after an abort: a partial count would lie, got {out:?}"
        );

        // Refusing Meta itself aborts before the scan even starts.
        let mut meta_seen = 0usize;
        let ok = handle_query_streaming(
            &ctx,
            "rs",
            0,
            false,
            false,
            std::time::Instant::now(),
            &mut |r| {
                if matches!(r, Response::Meta { .. }) {
                    meta_seen += 1;
                    return false;
                }
                panic!("nothing may be emitted after a refused Meta: {r:?}");
            },
            None,
            false,
        );
        assert!(!ok);
        assert_eq!(meta_seen, 1);

        // And the expr path aborts the same way.
        let mut out = Vec::new();
        let ok = handle_expr_streaming(
            &ctx,
            &serde_json::json!({"expression": ["suffix", "rs"]}),
            0,
            false,
            std::time::Instant::now(),
            &mut |r| {
                if matches!(r, Response::Hit { .. }) {
                    return false;
                }
                out.push(r);
                true
            },
            None,
            false,
        );
        assert!(!ok, "expr streaming must abort on a refused Hit");
        assert!(matches!(out.first(), Some(Response::Meta { .. })));
        assert!(!out.iter().any(|r| matches!(r, Response::Done { .. })));
    }

    #[test]
    fn unthrottled_scan_still_reports_moving_progress() {
        // The GUI's bug, reproduced end to end. On a real index a query like
        // `"comp"` finishes in ~30 ms; `progress_records_flow_between_meta_and_done`
        // above only passes because it *sleeps 60 ms per Progress record*,
        // slowing the scan artificially. Without that crutch the client gets
        // exactly one record — the `(0, total)` announcement — so the
        // percentage sits at 0% for the whole scan and never moves. That is
        // the reported symptom: a percent that never appears.
        //
        // A scan that reports only "0 of N" has told the client nothing
        // about how far along it is.
        let d = tempfile::tempdir().unwrap();
        let mut ix = Index::new();
        let basename = d.path().file_name().unwrap().to_string_lossy().to_string();
        let rid = ix.insert(
            ROOT_ID,
            basename.as_bytes(),
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        let src = ix.insert(rid, b"src", EntryFlags::EMPTY.with_dir(true));
        for i in 0..9000 {
            ix.insert(src, format!("file_{i:05}.rs").as_bytes(), EntryFlags::EMPTY);
        }
        let mut root = Root::new(0, d.path().to_path_buf());
        root.caps.writable = true;
        let shared = Shared::new(ix, vec![root]);
        let ctx = ServerCtx {
            shared,
            sync: SyncRegistry::new(),
            backend: "fake".into(),
            control: None,
            caller: None,
        };
        let mut out = Vec::new();
        // No sleeps anywhere: exactly how the pump drives a real reply.
        let ok = handle_query_streaming(
            &ctx,
            "rs",
            0,
            false,
            false,
            std::time::Instant::now(),
            &mut |r| {
                out.push(r);
                true
            },
            None,
            true,
        );
        assert!(ok);
        let progress: Vec<(u64, u64)> = out
            .iter()
            .filter_map(|r| match r {
                Response::Progress { scanned, total } => Some((*scanned, *total)),
                _ => None,
            })
            .collect();
        assert!(
            !progress.is_empty(),
            "an opt-in scan must report progress at all"
        );
        // The scan visited 9000+ entries; the client must learn more than
        // "we had not started". Any final record reporting 0 means the
        // percentage can never have moved off 0%.
        let last = *progress.last().expect("checked non-empty");
        assert!(
            last.0 > 0,
            "last progress record is {last:?}: the scan finished but the \
             client was only ever told 0 of {} — percent can never move. \
             Got {} record(s): {progress:?}",
            last.1,
            progress.len()
        );
    }

    #[test]
    fn progress_records_flow_between_meta_and_done() {
        // Opt-in: Meta, then Progress (monotone scanned, stable total),
        // Hits, then Done with the exact total. Opt-out: byte-identical to
        // the old shape (no Progress at all).
        let d = tempfile::tempdir().unwrap();
        let mut ix = Index::new();
        let basename = d.path().file_name().unwrap().to_string_lossy().to_string();
        let rid = ix.insert(
            ROOT_ID,
            basename.as_bytes(),
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        let src = ix.insert(rid, b"src", EntryFlags::EMPTY.with_dir(true));
        for i in 0..9000 {
            ix.insert(src, format!("file_{i:05}.rs").as_bytes(), EntryFlags::EMPTY);
        }
        let mut root = Root::new(0, d.path().to_path_buf());
        root.caps.writable = true;
        let shared = Shared::new(ix, vec![root]);
        let ctx = ServerCtx {
            shared,
            sync: SyncRegistry::new(),
            backend: "fake".into(),
            control: None,
            caller: None,
        };
        let run = |progress: bool| {
            let mut out = Vec::new();
            let ok = handle_query_streaming(
                &ctx,
                "rs",
                0,
                false,
                false,
                std::time::Instant::now(),
                &mut |r| {
                    let is_progress = matches!(r, Response::Progress { .. });
                    out.push(r);
                    // Slow the scan so the 100 ms rate gate lets several
                    // Progress records through (a 9k scan is ~10 ms).
                    if is_progress {
                        std::thread::sleep(std::time::Duration::from_millis(60));
                    }
                    true
                },
                None,
                progress,
            );
            assert!(ok);
            out
        };
        let without = run(false);
        assert!(
            !without.iter().any(|r| matches!(r, Response::Progress { .. })),
            "opt-out must not emit Progress"
        );
        // Opt-out shape is exactly the legacy Meta/Hit*/Done.
        assert!(matches!(without.first(), Some(Response::Meta { .. })));
        assert!(matches!(without.last(), Some(Response::Done { .. })));

        let with = run(true);
        assert!(matches!(with.first(), Some(Response::Meta { .. })));
        assert!(matches!(with.last(), Some(Response::Done { .. })));
        let mut seen_progress = false;
        let mut last_scanned = 0u64;
        let mut total = None;
        for r in &with {
            if let Response::Progress { scanned, total: t } = r {
                seen_progress = true;
                assert!(*scanned >= last_scanned, "scanned is monotone");
                last_scanned = *scanned;
                match total {
                    None => total = Some(*t),
                    Some(prev) => assert_eq!(prev, *t, "denominator stable"),
                }
            }
        }
        assert!(seen_progress, "a 9k opt-in scan must emit Progress");
        // Rate gate: ~10/s over a ~0.5 s scan (9 Progress × 60 ms sleeps)
        // means a handful of records, never hundreds.
        let n = with
            .iter()
            .filter(|r| matches!(r, Response::Progress { .. }))
            .count();
        assert!(n < 30, "rate gate must bound Progress count, got {n}");
        // Same hits and total either way: Progress changes timing, not answers.
        let hits = |out: &[Response]| {
            out.iter()
                .filter_map(|r| match r {
                    Response::Hit { path, .. } => Some(path.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(hits(&with), hits(&without));
        let done_count = |out: &[Response]| {
            out.iter().find_map(|r| match r {
                Response::Done { count, .. } => Some(*count),
                _ => None,
            })
        };
        assert_eq!(done_count(&with), done_count(&without));
        assert_eq!(total, Some(ctx.shared.index.read().unwrap().live_count()));
    }

    #[test]
    fn streaming_abort_poll_stops_the_scan_without_any_refusal() {
        // The poll fires even when every write succeeds: no refusal, no
        // `Done`, scan halted. This is the server half of the daemon's
        // `client_gone` poll — zero-hit scans and buffered writes included.
        //
        // The poll fires per 4096-entry chunk, so the index must span a
        // chunk boundary: a tiny tree finishes before the first poll, which
        // is correct behavior (no overshoot to bound). 5000 entries forces
        // at least one poll mid-scan.
        let d = tempfile::tempdir().unwrap();
        let mut ix = Index::new();
        let basename = d.path().file_name().unwrap().to_string_lossy().to_string();
        let rid = ix.insert(
            ROOT_ID,
            basename.as_bytes(),
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        let src = ix.insert(rid, b"src", EntryFlags::EMPTY.with_dir(true));
        for i in 0..5000 {
            ix.insert(src, format!("file_{i:05}.rs").as_bytes(), EntryFlags::EMPTY);
        }
        let mut root = Root::new(0, d.path().to_path_buf());
        root.caps.writable = true;
        let shared = Shared::new(ix, vec![root]);
        let ctx = ServerCtx {
            shared,
            sync: SyncRegistry::new(),
            backend: "fake".into(),
            control: None,
            caller: None,
        };
        let mut out = Vec::new();
        let ok = handle_query_streaming(
            &ctx,
            "rs",
            0,
            false,
            false,
            std::time::Instant::now(),
            &mut |r| {
                out.push(r);
                true
            },
            Some(&|| false),
            false,
        );
        assert!(!ok, "an abort poll must halt the streaming reply");
        assert!(
            matches!(out.first(), Some(Response::Meta { .. })),
            "Meta went out before the poll fired, got {out:?}"
        );
        assert!(
            !out.iter().any(|r| matches!(r, Response::Done { .. })),
            "no Done after a poll abort, got {out:?}"
        );
    }

    #[test]
    fn expression_query_reaches_expression_engine() {
        // `["allof", ["suffix","rs"]]` should find src/main.rs via the
        // Watchman-style expression path.
        let (ctx, _d, _s) = ctx_with_tree();
        let req = Request::Query {
            q: String::new(),
            limit: 0,
            json: true,
            count: false,
            expression: Some(serde_json::json!(["suffix", "rs"])),
            glob: None,
            glob_includedotfiles: None,
            glob_noescape: None,
            suffix: None,
            path: None,
            relative_root: None,
            case_sensitive: None,
            fuzzy: false,
            max_distance: None,
        progress: false,
        };
        let out = handle_request(&ctx, &req);
        let hits: Vec<_> = out
            .iter()
            .filter_map(|r| match r {
                Response::Hit { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(hits.len(), 1, "got {hits:?}");
        assert!(hits[0].ends_with("/src/main.rs"));
    }

    #[test]
    fn expression_query_errors_are_reported() {
        let (ctx, _d, _s) = ctx_with_tree();
        let req = Request::Query {
            q: String::new(),
            limit: 0,
            json: false,
            count: false,
            expression: Some(serde_json::json!(["type", "x"])),
            glob: None,
            glob_includedotfiles: None,
            glob_noescape: None,
            suffix: None,
            path: None,
            relative_root: None,
            case_sensitive: None,
            fuzzy: false,
            max_distance: None,
        progress: false,
        };
        let out = handle_request(&ctx, &req);
        match &out[0] {
            Response::Error { message } => {
                assert!(message.contains("invalid type string 'x'"), "{message}")
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn since_stale_generation_is_fresh() {
        let (ctx, _d, shared) = ctx_with_tree();
        shared
            .generation
            .store(9, std::sync::atomic::Ordering::Release);
        let out = handle_request(
            &ctx,
            &Request::Since {
                cursor: "c:1:0".into(),
                q: String::new(),
            },
        );
        match out.first() {
            Some(Response::Meta { fresh, gen, .. }) => {
                assert!(fresh, "generation mismatch must report fresh");
                assert_eq!(*gen, 9);
            }
            _ => panic!("expected meta"),
        }
    }

    #[test]
    fn stats_includes_entries() {
        let (ctx, _d, _s) = ctx_with_tree();
        let out = handle_request(&ctx, &Request::Stats { roots: true });
        match &out[0] {
            Response::Stats { stats } => {
                assert_eq!(stats["entries"], 4);
                assert_eq!(stats["backend"], "fake");
                assert!(stats["roots"].is_array());
            }
            _ => panic!("expected stats"),
        }
    }

    #[test]
    fn rescan_unknown_root_errors() {
        let (ctx, _d, _s) = ctx_with_tree();
        let out = handle_request(
            &ctx,
            &Request::Rescan {
                root: "/nope".into(),
            },
        );
        assert!(matches!(out[0], Response::Error { .. }));
    }

    #[test]
    fn sync_without_control_reports_weak() {
        let (ctx, _d, _s) = ctx_with_tree();
        let out = handle_request(
            &ctx,
            &Request::Sync {
                timeout_ms: 100,
                roots: None,
            },
        );
        match &out[0] {
            Response::Synced { roots } => {
                assert_eq!(roots.len(), 1);
                assert!(
                    roots.values().all(|v| v == "weak"),
                    "read-only fallback must not claim ok"
                );
            }
            _ => panic!("expected synced"),
        }
    }

    #[test]
    fn cookie_sync_completes_when_marker_seen() {
        // Simulate the daemon: a control channel that registers the cookie, then
        // an event carrying the cookie's create. The sync must report "ok".
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("tree");
        std::fs::create_dir_all(&root).unwrap();

        let mut ix = Index::new();
        let basename = root.file_name().unwrap().to_string_lossy().to_string();
        let rid = ix.insert(
            ROOT_ID,
            basename.as_bytes(),
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        let mut r = Root::new(0, root.clone());
        r.caps.writable = true;
        let shared = Shared::new(ix, vec![r]);
        let sync = SyncRegistry::new();
        let (tx, rx) = crossbeam_channel::bounded::<Control>(8);

        // Applier thread: mimics fflocated's tick loop.
        let applier_shared = shared.clone();
        let applier_sync = sync.clone();
        let handle = std::thread::spawn(move || {
            use crate::pending::Pending;
            let mut pending = Pending::new(100);
            while let Ok(ctl) = rx.recv_timeout(std::time::Duration::from_millis(500)) {
                match ctl {
                    Control::Cookie { token, root } => {
                        // Register the cookie under the root dir id.
                        let name = crate::sync::cookie_name(token);
                        pending.register_cookie(token, root, name.clone());
                        // The watcher delivers the create next.
                        pending.push_event(crate::event::FsEvent::upsert(
                            crate::event::DirKey::new(root),
                            name,
                            false,
                        ));
                        // Drain: cookie is seen + queues empty → complete.
                        while let Some(ev) = pending.pop_event() {
                            let _ = ev;
                        }
                        for c in pending.take_completed_cookies() {
                            // Mirror the real applier's `check_cookies`: grab the
                            // path before completing, then unlink.
                            let path = applier_sync.cookie_path(c.token);
                            applier_sync.complete(c.token);
                            if let Some(p) = path {
                                let _ = std::fs::remove_file(p);
                            }
                        }
                    }
                    Control::RescanRoot { .. } => {}
                }
                let _ = applier_shared.generation();
            }
        });

        let ctx = ServerCtx {
            shared,
            sync,
            backend: "fake".into(),
            control: Some(tx),
            caller: None,
        };
        let out = handle_request(
            &ctx,
            &Request::Sync {
                timeout_ms: 1000,
                roots: None,
            },
        );
        match &out[0] {
            Response::Synced { roots } => {
                let v: Vec<&String> = roots.values().collect();
                assert_eq!(
                    v,
                    vec![&"ok".to_string()],
                    "cookie marker seen → sync ok, got {roots:?}"
                );
            }
            other => panic!("expected synced, got {other:?}"),
        }
        // The cookie file must have been cleaned up by the sync path. The
        // applier completes the waiter just before unlinking, so poll
        // briefly: asserting once would race the unlink (flaky under load,
        // routinely observed on Windows).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let littered = std::fs::read_dir(&root)
                .unwrap()
                .filter_map(|e| e.ok())
                .any(|e| crate::util::is_cookie(e.file_name().as_encoded_bytes()));
            if !littered {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "cookie file was never cleaned up"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        drop(ctx);
        let _ = handle.join();
        let _ = rid;
    }

    #[cfg(feature = "trigram")]
    #[test]
    fn fuzzy_query_finds_a_typo_via_the_protocol() {
        // The tree has src/main.rs and README.md. "main.js" is 1 edit from
        // "main.rs": an exact query misses, a fuzzy query finds and ranks it.
        let (ctx, _d, _s) = ctx_with_tree();
        let exact = handle_request(&ctx, &Request::query("main.js", 0, false, false));
        assert!(matches!(
            exact.last(),
            Some(Response::Done { count: 0, .. })
        ));

        let req = Request::Query {
            q: "main.js".into(),
            limit: 0,
            json: false,
            count: false,
            expression: None,
            glob: None,
            glob_includedotfiles: None,
            glob_noescape: None,
            suffix: None,
            path: None,
            relative_root: None,
            case_sensitive: None,
            fuzzy: true,
            max_distance: Some(1),
        progress: false,
        };
        let out = handle_request(&ctx, &req);
        assert!(matches!(out.first(), Some(Response::Meta { .. })));
        let hits: Vec<_> = out
            .iter()
            .filter_map(|r| match r {
                Response::Hit { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(hits.len(), 1, "got {hits:?}");
        assert!(hits[0].ends_with("/src/main.rs"), "got {hits:?}");
    }

    #[cfg(feature = "trigram")]
    #[test]
    fn fuzzy_query_is_deterministic_across_repeats() {
        let (ctx, _d, _s) = ctx_with_tree();
        let mk = || Request::Query {
            q: "main.js".into(),
            limit: 0,
            json: false,
            count: false,
            expression: None,
            glob: None,
            glob_includedotfiles: None,
            glob_noescape: None,
            suffix: None,
            path: None,
            relative_root: None,
            case_sensitive: None,
            fuzzy: true,
            max_distance: Some(2),
        progress: false,
        };
        let run = || {
            let out = handle_request(&ctx, &mk());
            out.iter()
                .filter_map(|r| match r {
                    Response::Hit { path, .. } => Some(path.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let first = run();
        for _ in 0..5 {
            assert_eq!(first, run(), "fuzzy ranking must be reproducible");
        }
    }

    #[cfg(feature = "trigram")]
    #[test]
    fn fuzzy_count_only_reports_the_ranked_total() {
        let (ctx, _d, _s) = ctx_with_tree();
        let req = Request::Query {
            q: "main.js".into(),
            limit: 0,
            json: false,
            count: true,
            expression: None,
            glob: None,
            glob_includedotfiles: None,
            glob_noescape: None,
            suffix: None,
            path: None,
            relative_root: None,
            case_sensitive: None,
            fuzzy: true,
            max_distance: Some(1),
        progress: false,
        };
        let out = handle_request(&ctx, &req);
        assert!(
            !out.iter().any(|r| matches!(r, Response::Hit { .. })),
            "--count skips hits"
        );
        let count = out.iter().find_map(|r| match r {
            Response::Done { count, .. } => Some(*count),
            _ => None,
        });
        assert_eq!(count, Some(1));
    }
}
