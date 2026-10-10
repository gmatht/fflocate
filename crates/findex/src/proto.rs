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

pub const PROTOCOL_VERSION: u32 = 2;

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
        #[serde(skip_serializing_if = "Option::is_none")]
        backend: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        entries: Option<u64>,
    },
    /// One search hit.
    Hit { path: String, id: u32 },
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
    Pong { pid: u32, version: u32, backend: String },
}

impl Response {
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{\"type\":\"error\",\"message\":\"serialize\"}".into())
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
        let r = Request::from_line(r#"{"v":2,"op":"query","q":"src ext:rs","limit":50,"json":true}"#).unwrap();
        match r {
            Request::Query { q, limit, json, count, .. } => {
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
        assert!(matches!(Request::from_line(r#"{"op":"hello"}"#).unwrap(), Request::Hello));
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
        };
        let line = m.to_line();
        assert!(line.contains("\"type\":\"meta\""));
        assert!(line.contains("\"gen\":7"));
        let d = Response::Done { count: 412, cursor: "c:7:481221".into() };
        assert!(d.to_line().contains("\"count\":412"));
    }

    #[test]
    fn synced_shape_matches_design() {
        let mut roots = std::collections::BTreeMap::new();
        roots.insert("default".to_string(), "ok".to_string());
        roots.insert("/mnt/ro".to_string(), "weak".to_string());
        let line = Response::Synced { roots }.to_line();
        assert!(line.contains("\"type\":\"synced\""));
        assert!(line.contains("\"default\":\"ok\""));
        assert!(line.contains("\"weak\""));
    }
}
