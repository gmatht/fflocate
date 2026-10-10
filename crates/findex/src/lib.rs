//! findex core library.
//!
//! Modules map onto the design document:
//!
//! * [`index`]     — §3 index data model (entries, names arena, tombstone flags)
//! * [`roots`]     — §3/§4 roots table and per-mount capabilities
//! * [`journal`]   — §9 journal: the append-only log that is also the clock
//! * [`snapshot`]  — §9 FNX2 snapshot file format
//! * [`persist`]   — §9 snapshot + journal wiring, load/replay/compaction
//! * [`query`]     — §10 query engine
//! * `trigram`     — §10/§17 v2 trigram posting-list accelerator (query prefilter;
//!   behind the default-on `trigram` cargo feature)
//! * [`cursor`]    — §9 `c:<gen>:<lsn>` cursors
//! * [`event`]     — §6 internal `FsEvent` language
//! * [`watch`]     — §6 `WatchBackend` trait, fake backend, inotify backend
//! * [`pending`]   — §7 pending collection
//! * [`applier`]   — §7/§13 single-writer applier, rescans, backoff, degraded mode
//! * [`crawl`]     — §5 initial crawl
//! * [`sync`]      — §8 cookie sync
//! * [`proto`]     — §11 IPC protocol v2 (NDJSON messages)
//! * [`server`]    — §8/§11 sync engine + request handling shared by the daemon
//! * [`health`]    — §15 health counters/telemetry

pub mod applier;
pub mod config_paths;
pub mod crawl;
pub mod cursor;
pub mod drive;
pub mod event;
pub mod excludes;
pub mod expr;
pub mod fuzzy;
pub mod health;
pub mod index;
pub mod journal;
pub mod pending;
pub mod modelfs;
pub mod mount;
pub mod ownership;
pub mod persist;
pub mod proto;
pub mod query;
pub mod roots;
pub mod server;
pub mod snapshot;
pub mod sync;
#[cfg(feature = "trigram")]
pub mod trigram;
pub mod transport;
pub mod util;
pub mod watch;

pub use cursor::Cursor;
pub use event::{DirKey, FsEvent, RescanCause, RootId};
pub use index::{Entry, EntryFlags, Index, ROOT_ID};


pub use roots::{MountCaps, Root, RootState};
