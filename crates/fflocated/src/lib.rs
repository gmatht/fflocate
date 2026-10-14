//! fflocated: the fflocate daemon.
//!
//! Split into a library (so tests and the CLI's `--foreground` path can drive
//! it) and a thin binary.

pub mod config;
pub mod daemon;
pub mod net;
