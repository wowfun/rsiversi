//! Experimental v0 daemon and local transport adapters.
//!
//! Command-line behavior and the Rust module surface may change between
//! `rsi-meta` releases without compatibility guarantees.

#![cfg(unix)]
#![deny(unsafe_code)]

mod auth;
pub mod cli;
mod composition;
mod framing;
mod host;
mod http;
mod lifecycle;
pub mod protocol;
mod streams;
#[cfg(feature = "test-failpoints")]
mod test_failpoints;
mod unix;

/// Process exit used when an online apply requires an external stop/install cycle.
pub const DAEMON_RESTART_EXIT_CODE: u8 = 75;
