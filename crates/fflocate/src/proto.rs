//! IPC protocol v2 (§11): NDJSON over a unix socket.
//!
//! Requests are single JSON objects; responses are one or more JSON objects,
//! one per line. A `query` streams `meta`, then `hit`* then `done`. A `sync`
//! answers with a single `synced`. A `since` answers `meta` then deltas.
//!
//! Design rules carried from Watchman: coalesce per subscriber, slow clients
//! catch up or resync (a stale generation yields `fresh: true` and a full
//! listing), and never let a client block the applier.

use serde::{Deserialize, Serialize};

/// Wire protocol version.
///
/// Bumped whenever the *meaning* of a field changes, not only when the shape
/// does: a client that sends `case_sensitive` depends on the daemon honouring it
/// on the plain-string query path, and v2 daemons built before that silently
/// ignored it. v3 marks the behaviour change so an old daemon rejects the
/// request instead of quietly doing the wrong thing.
pub const PROTOCOL_VERSION: u32 = 3;

/// A client request.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Request {
    /// Search the index.
    Query {
        #[serde(default)]
        q: String,
        #[serde(default)]
        limit: usize,
        #[serde(default)]
        json: bool,
        /// Return only a count (skips path materialization, §10).
        #[serde(default)]
        count: bool,
        /// Watchman-style expression tree (`["allof", ["type","f"], ...]`).
        /// When present, it takes precedence over the string `q` (§10, expr).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expression: Option<serde_json::Value>,
        /// Watchman-style generators; only meaningful with `expression`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        glob: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        glob_includedotfiles: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        glob_noescape: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        suffix: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<serde_json::Value>,
        /// Subtree to scope the query to; names are reported relative to it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        relative_root: Option<String>,
        /// Override the filesystem's case sensitivity.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        case_sensitive: Option<bool>,
        /// Approximate (edit-distance) matching for the plain substring terms
        /// (§10/§17 v2). Off by default; when on, results are ranked by score.
        #[serde(default)]
        fuzzy: bool,
        /// Maximum edit distance for `fuzzy` (defaults server-side when absent).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_distance: Option<u32>,
        /// Ask the daemon to emit `Progress` records during the scan
        /// (scan-fraction progress for the GUI's percent display).
        /// Opt-in: off by default, so fast queries and old clients pay
        /// nothing and see a byte-identical reply.
        #[serde(default)]
        progress: bool,
    },
    /// Settle barrier: complete when everything before the call is indexed (§8).
    Sync {
        #[serde(default = "default_sync_timeout")]
        timeout_ms: u64,
        #[serde(default)]
        roots: Option<Vec<String>>,
    },
    /// Delta events since a cursor (§9). A stale generation yields `fresh: true`.
    Since {
        cursor: String,
        #[serde(default)]
        q: String,
    },
    /// Live change feed (v1.5 preview).
    Subscribe {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        cursor: Option<String>,
    },
    /// Health telemetry (§15).
    Stats {
        #[serde(default)]
        roots: bool,
    },
    /// Force a rescan of a root.
    Rescan { root: String },
    /// Ping for auto-spawn liveness checks.
    Hello,
}

fn default_sync_timeout() -> u64 {
    2000
}

/// A server response line.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Response {
    /// Metadata preamble for query/since/subscribe/stats.
    Meta {
        gen: u64,
        lsn: u64,
        fresh: bool,
        elapsed_ms: f64,
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        stale_roots: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        catching_up: Option<bool>,
        /// How far the daemon is behind the filesystem, in ms: the age of the
        /// oldest still-unapplied change (queued event/work item, or a
        /// still-draining journal-replay tail). Absent/`None` when caught up.
        ///
        /// Computed at reply time from a stored arrival timestamp, so the age
        /// keeps growing between ticks rather than freezing at the last tick's
        /// value. An older daemon omits the field; a newer client then shows no
        /// lag rather than inventing one.
        #[serde(skip_serializing_if = "Option::is_none")]
        lag_ms: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        backend: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        entries: Option<u64>,
    },
    /// One search hit.
    Hit { path: String, id: u32 },
    /// Scan-fraction progress: `scanned` of `total` index entries visited so
    /// far (entries *visited*, including tombstones and skips — not matches).
    /// Emitted only when the query asked with `progress: true`, between `Meta`
    /// and `Done`; never terminal, never required. A client that sees none
    /// renders seconds-only, exactly as before.
    Progress { scanned: u64, total: u64 },
    /// End of a query.
    Done { count: u64, cursor: String },
    /// One delta event since a cursor.
    Event {
        cursor: String,
        action: String,
        path: String,
    },
    /// Sync completed.
    Synced {
        roots: std::collections::BTreeMap<String, String>,
    },
    /// Stats payload.
    Stats { stats: serde_json::Value },
    /// Acknowledgement of a rescan.
    Rescanning { root: String },
    /// An error, terminating the request.
    Error { message: String },
    /// Handshake answer.
    Pong {
        pid: u32,
        version: u32,
        backend: String,
    },
}

impl Response {
    pub fn to_line(&self) -> String {
        serde_json::to_string(self)
            .unwrap_or_else(|_| "{\"type\":\"error\",\"message\":\"serialize\"}".into())
    }
}

impl Request {
    /// Convenience constructor for a plain string query (all expression fields
    /// unset). Keeps existing call sites terse.
    pub fn query(q: impl Into<String>, limit: usize, json: bool, count: bool) -> Request {
        Request::Query {
            q: q.into(),
            limit,
            json,
            count,
            expression: None,
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
        }
    }

    pub fn from_line(line: &str) -> Result<Request, serde_json::Error> {
        let v: serde_json::Value = serde_json::from_str(line)?;
        // Accept requests with or without the `v` field; validate version when
        // present.
        if let Some(ver) = v.get("v").and_then(|x| x.as_u64()) {
            if ver != PROTOCOL_VERSION as u64 {
                return Err(serde_json::Error::io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unsupported protocol version {ver}"),
                )));
            }
        }
        serde_json::from_value(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_query() {
        let r =
            Request::from_line(r#"{"v":3,"op":"query","q":"src ext:rs","limit":50,"json":true}"#)
                .unwrap();
        match r {
            Request::Query {
                q,
                limit,
                json,
                count,
                ..
            } => {
                assert_eq!(q, "src ext:rs");
                assert_eq!(limit, 50);
                assert!(json);
                assert!(!count);
            }
            _ => panic!("wrong request"),
        }
    }

    #[test]
    fn parse_sync_defaults_timeout() {
        let r = Request::from_line(r#"{"op":"sync"}"#).unwrap();
        match r {
            Request::Sync { timeout_ms, roots } => {
                assert_eq!(timeout_ms, 2000);
                assert!(roots.is_none());
            }
            _ => panic!("wrong request"),
        }
    }

    #[test]
    fn parse_since_and_stats() {
        assert!(matches!(
            Request::from_line(r#"{"op":"since","cursor":"c:7:9001","q":""}"#).unwrap(),
            Request::Since { .. }
        ));
        assert!(matches!(
            Request::from_line(r#"{"op":"stats","roots":true}"#).unwrap(),
            Request::Stats { roots: true }
        ));
        assert!(matches!(
            Request::from_line(r#"{"op":"hello"}"#).unwrap(),
            Request::Hello
        ));
    }

    #[test]
    fn wrong_version_rejected() {
        assert!(Request::from_line(r#"{"v":1,"op":"query","q":"x"}"#).is_err());
    }

    #[test]
    fn unknown_op_rejected() {
        assert!(Request::from_line(r#"{"op":"frobnicate"}"#).is_err());
    }

    #[test]
    fn response_lines_roundtrip() {
        let m = Response::Meta {
            gen: 7,
            lsn: 481221,
            fresh: false,
            elapsed_ms: 7.1,
            stale_roots: vec![],
            catching_up: None,
            backend: Some("inotify".into()),
            entries: Some(10),
            lag_ms: None,
        };
        let line = m.to_line();
        assert!(line.contains("\"type\":\"meta\""));
        assert!(line.contains("\"gen\":7"));
        assert!(
            !line.contains("lag_ms"),
            "caught up must omit lag_ms entirely: {line}"
        );
        // A behind daemon reports the age, and an old client's line (no
        // field) parses as "no lag known" rather than erroring.
        let behind = Response::Meta {
            gen: 7,
            lsn: 481221,
            fresh: false,
            elapsed_ms: 7.1,
            stale_roots: vec![],
            catching_up: Some(true),
            lag_ms: Some(4200),
            backend: Some("inotify".into()),
            entries: Some(10),
        };
        let behind_line = behind.to_line();
        assert!(
            behind_line.contains("\"lag_ms\":4200"),
            "behind daemon carries the age: {behind_line}"
        );
        let old: Response = serde_json::from_str(
            r#"{"type":"meta","gen":7,"lsn":1,"fresh":false,"elapsed_ms":1.0}"#,
        )
        .unwrap();
        match old {
            Response::Meta { lag_ms, .. } => assert_eq!(lag_ms, None),
            other => panic!("expected meta, got {other:?}"),
        }
        let d = Response::Done {
            count: 412,
            cursor: "c:7:481221".into(),
        };
        assert!(d.to_line().contains("\"count\":412"));
    }

    #[test]
    fn progress_flag_roundtrips_and_defaults_off() {
        // Opt-in: the flag is always on the wire (like `fuzzy`), and an old
        // line without it parses with progress off — an old daemon's reply
        // shape and a new client's default both mean "no Progress records".
        let r = Request::from_line(r#"{"v":3,"op":"query","q":"comp","progress":true}"#).unwrap();
        match r {
            Request::Query { progress, .. } => assert!(progress),
            _ => panic!("wrong request"),
        }
        let r = Request::from_line(r#"{"v":3,"op":"query","q":"comp"}"#).unwrap();
        match r {
            Request::Query { progress, .. } => assert!(!progress),
            _ => panic!("wrong request"),
        }
        // The convenience constructor never opts in.
        match Request::query("x", 0, false, false) {
            Request::Query { progress, .. } => assert!(!progress),
            _ => panic!("wrong request"),
        }
    }

    #[test]
    fn progress_record_roundtrips() {
        let p = Response::Progress {
            scanned: 4096,
            total: 2214992,
        };
        let line = p.to_line();
        assert!(line.contains("\"type\":\"progress\""), "{line}");
        assert!(line.contains("\"scanned\":4096"), "{line}");
        let back: Response = serde_json::from_str(&line).unwrap();
        assert!(matches!(
            back,
            Response::Progress {
                scanned: 4096,
                total: 2214992
            }
        ));
    }

    #[test]
    fn synced_shape_matches_design() {        let mut roots = std::collections::BTreeMap::new();
        roots.insert("default".to_string(), "ok".to_string());
        roots.insert("/mnt/ro".to_string(), "weak".to_string());
        let line = Response::Synced { roots }.to_line();
        assert!(line.contains("\"type\":\"synced\""));
        assert!(line.contains("\"default\":\"ok\""));
        assert!(line.contains("\"weak\""));
    }
}
