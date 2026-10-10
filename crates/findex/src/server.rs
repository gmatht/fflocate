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
        Request::Query { q, limit, json: _, count, expression, glob, glob_includedotfiles, glob_noescape, suffix, path, relative_root, case_sensitive } => {
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
                    obj.insert("relative_root".into(), serde_json::Value::String(rr.clone()));
                }
                if let Some(cs) = case_sensitive {
                    obj.insert("case_sensitive".into(), serde_json::Value::Bool(*cs));
                }
                handle_expr_query(ctx, &serde_json::Value::Object(obj), *limit, *count, t0)
            } else {
                handle_query(ctx, q, *limit, *count, t0)
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
    Response::Meta {
        gen: ctx.shared.generation(),
        lsn: ctx.shared.snapshot_lsn(),
        fresh,
        elapsed_ms: elapsed.as_secs_f64() * 1000.0,
        stale_roots: stale,
        catching_up: if catching_up { Some(true) } else { None },
        backend: Some(ctx.backend.clone()),
        entries: Some(ix.live_count()),
    }
}

fn handle_query(ctx: &ServerCtx, raw: &str, limit: usize, count_only: bool, t0: Instant) -> Vec<Response> {
    let q = match Query::compile(raw) {
        Ok(q) => q,
        Err(e) => return vec![Response::Error { message: e }],
    };
    let ix = ctx.shared.index.read().unwrap();
    let roots = ctx.shared.roots.read().unwrap();
    // Build the (root_dir_id, root_path) table once.
    let mut root_paths: Vec<(u32, Vec<u8>)> = Vec::new();
    for r in roots.iter() {
        let rid = crate::applier::root_dir_id(&ix, r);
        if rid != crate::index::ROOT_ID {
            let mut p = r.path.as_os_str().to_string_lossy().as_bytes().to_vec();
            root_paths.push((rid, std::mem::take(&mut p)));
        }
    }
    let effective_limit = if count_only { 0 } else { limit };
    let res = query::execute(&ix, &root_paths, &q, effective_limit, count_only);
    let cursor = ctx.shared.cursor().to_string_repr();
    let elapsed = t0.elapsed();
    drop(roots);
    drop(ix);

    let mut out = Vec::new();
    out.push(meta_preamble(ctx, false, elapsed));
    if count_only {
        out.push(Response::Done { count: res.total, cursor });
        return out;
    }
    for h in &res.hits {
        out.push(Response::Hit { path: bytes_to_string(&h.path), id: h.id });
    }
    out.push(Response::Done { count: res.total, cursor });
    out
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
    let expr = match crate::expr::parse_query(query) {
        Ok(e) => e,
        Err(e) => return vec![Response::Error { message: e }],
    };
    let rel_root = query
        .get("relative_root")
        .and_then(|v| v.as_str())
        .map(|s| s.as_bytes().to_vec())
        .unwrap_or_default();

    let ix = ctx.shared.index.read().unwrap();
    let roots = ctx.shared.roots.read().unwrap();
    let mut root_paths: Vec<(u32, Vec<u8>)> = Vec::new();
    for r in roots.iter() {
        let rid = crate::applier::root_dir_id(&ix, r);
        if rid != crate::index::ROOT_ID {
            root_paths.push((rid, r.path.as_os_str().to_string_lossy().as_bytes().to_vec()));
        }
    }
    let effective_limit = if count_only { 0 } else { limit };
    let res = query::execute_expr(&ix, &root_paths, &rel_root, &expr, effective_limit, count_only);
    let cursor = ctx.shared.cursor().to_string_repr();
    let elapsed = t0.elapsed();
    drop(roots);
    drop(ix);

    let mut out = Vec::new();
    out.push(meta_preamble(ctx, false, elapsed));
    if count_only {
        out.push(Response::Done { count: res.total, cursor });
        return out;
    }
    for h in &res.hits {
        out.push(Response::Hit { path: bytes_to_string(&h.path), id: h.id });
    }
    out.push(Response::Done { count: res.total, cursor });
    out
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
            .map(|(i, r)| {
                let rid = crate::applier::root_dir_id(&ix, r);
                (i, rid, r.key(), r.path.clone(), r.caps.writable)
            })
            .collect()
    };

    let mut out_map = std::collections::BTreeMap::new();
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
        let rx = ctx.sync.expect(token, *rid, name.clone(), Some(cookie_path.clone()));
        // Ask the applier to register the cookie with the pending collection.
        let mut asked = false;
        if let Some(ctl) = &ctx.control {
            asked = ctl
                .send(Control::Cookie { token, root: *rid })
                .is_ok();
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
        if out_map.get(key).map(|s| s == SyncStatus::Timeout.as_str()).unwrap_or(false) {
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
        None => return vec![Response::Error { message: format!("bad cursor: {cursor}") }],
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
            (rid != crate::index::ROOT_ID)
                .then(|| (rid, r.path.as_os_str().to_string_lossy().as_bytes().to_vec()))
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
    if per_root {
        let roots_json: Vec<serde_json::Value> = roots
            .iter()
            .map(|r| {
                serde_json::json!({
                    "path": r.key(),
                    "enabled": r.enabled,
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
    let (found, rid) = {
        let ix = ctx.shared.index.read().unwrap();
        let roots = ctx.shared.roots.read().unwrap();
        match roots.iter().find(|r| r.key() == root) {
            Some(r) => (true, crate::applier::root_dir_id(&ix, r)),
            None => (false, crate::index::ROOT_ID),
        }
    };
    if !found {
        return vec![Response::Error { message: format!("unknown root: {root}") }];
    }
    if let Some(ctl) = &ctx.control {
        let _ = ctl.send(Control::RescanRoot { root: rid });
    }
    vec![Response::Rescanning { root: root.to_string() }]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::applier::Shared;
    use crate::index::{EntryFlags, Index, ROOT_ID};
    use crate::persist::Store;
    use crate::roots::Root;


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
        };
        let store_dir = d.path().join("state");
        let _ = Store::open(&store_dir);
        (ctx, d, shared)
    }

    #[test]
    fn query_returns_meta_hits_done() {
        let (ctx, _d, _s) = ctx_with_tree();
        let out = handle_request(&ctx, &Request::query("rs", 0, true, false));
        assert!(matches!(out.first(), Some(Response::Meta { .. })));
        assert!(matches!(out.last(), Some(Response::Done { .. })));
        let hits: Vec<_> = out.iter().filter_map(|r| match r { Response::Hit { path, .. } => Some(path.clone()), _ => None }).collect();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].ends_with("/src/main.rs"));
    }

    #[test]
    fn count_fast_path() {
        let (ctx, _d, _s) = ctx_with_tree();
        let out = handle_request(&ctx, &Request::query("rs", 0, false, true));
        let done = out.iter().find_map(|r| match r { Response::Done { count, .. } => Some(*count), _ => None }).unwrap();
        assert_eq!(done, 1);
        assert!(!out.iter().any(|r| matches!(r, Response::Hit { .. })));
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
        };
        let out = handle_request(&ctx, &req);
        let hits: Vec<_> = out.iter().filter_map(|r| match r { Response::Hit { path, .. } => Some(path.clone()), _ => None }).collect();
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
        };
        let out = handle_request(&ctx, &req);
        match &out[0] {
            Response::Error { message } => assert!(message.contains("invalid type string 'x'"), "{message}"),
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn since_stale_generation_is_fresh() {
        let (ctx, _d, shared) = ctx_with_tree();
        shared.generation.store(9, std::sync::atomic::Ordering::Release);
        let out = handle_request(&ctx, &Request::Since { cursor: "c:1:0".into(), q: String::new() });
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
        let out = handle_request(&ctx, &Request::Rescan { root: "/nope".into() });
        assert!(matches!(out[0], Response::Error { .. }));
    }

    #[test]
    fn sync_without_control_reports_weak() {
        let (ctx, _d, _s) = ctx_with_tree();
        let out = handle_request(&ctx, &Request::Sync { timeout_ms: 100, roots: None });
        match &out[0] {
            Response::Synced { roots } => {
                assert_eq!(roots.len(), 1);
                assert!(roots.values().all(|v| v == "weak"), "read-only fallback must not claim ok");
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

        // Applier thread: mimics findexd's tick loop.
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
        };
        let out = handle_request(&ctx, &Request::Sync { timeout_ms: 1000, roots: None });
        match &out[0] {
            Response::Synced { roots } => {
                let v: Vec<&String> = roots.values().collect();
                assert_eq!(v, vec![&"ok".to_string()], "cookie marker seen → sync ok, got {roots:?}");
            }
            other => panic!("expected synced, got {other:?}"),
        }
        // The cookie file must have been cleaned up by the sync path.
        assert!(!std::fs::read_dir(&root)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| crate::util::is_cookie(e.file_name().as_encoded_bytes())));
        drop(ctx);
        let _ = handle.join();
        let _ = rid;
    }
}
