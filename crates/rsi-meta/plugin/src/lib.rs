//! Fixed-layout native ABI v2 wire types for trusted in-process plugins.
//!
//! The ABI has one exchange port per side. It carries fixed-width integers,
//! opaque slot-plus-epoch capabilities, borrowed pointer/length ranges, and
//! issuer-owned one-shot output release tokens. Context, Fiber, routing, and
//! cleanup policy remain in the host.

#![deny(unsafe_op_in_unsafe_fn, clippy::undocumented_unsafe_blocks)]
#![allow(unsafe_code)] // This crate is the deliberately audited C ABI boundary.
#![allow(clippy::missing_errors_doc)]

#[cfg(not(target_pointer_width = "64"))]
compile_error!("rsi-meta native ABI v2 requires 64-bit pointers");

mod sdk;
mod wire;

pub use sdk::*;
pub use wire::*;

pub const ABI_MAJOR: u32 = 2;
pub const ABI_MINOR: u32 = 0;
pub const MAX_DIAGNOSTIC_BYTES: usize = 64 * 1024;
pub const PLUGIN_ENTRY_SYMBOL: &[u8] = b"rsi_meta_plugin_entry_v2\0";

pub const STATUS_OK: u32 = 0;
pub const STATUS_INVALID_ARGUMENT: u32 = 1;
pub const STATUS_FAILED: u32 = 2;
pub const STATUS_PANICKED: u32 = 3;
pub const STATUS_PROTOCOL_ERROR: u32 = 4;
pub const STATUS_UNSUPPORTED: u32 = 5;
pub const STATUS_BUSY: u32 = 6;
pub const STATUS_REENTRANT: u32 = 7;
pub const STATUS_STALE_CAPABILITY: u32 = 8;
pub const STATUS_WRONG_CAPABILITY: u32 = 9;
pub const STATUS_LIMIT_EXCEEDED: u32 = 10;
pub const STATUS_CANCELLED: u32 = 11;
pub const STATUS_TERMINAL: u32 = 12;
pub const STATUS_BUFFER_TOO_SMALL: u32 = 13;

pub const PLUGIN_IDENTITY: u32 = 1;
pub const PLUGIN_PREPARE: u32 = 2;
pub const PLUGIN_CREATE: u32 = 3;
pub const PLUGIN_ACTIVATE: u32 = 4;
pub const PLUGIN_SERVE_PORT: u32 = 5;
pub const PLUGIN_RUN_CLEANUP: u32 = 6;
pub const PLUGIN_DESTROY_INSTANCE: u32 = 7;
pub const PLUGIN_DESTROY_FACTORY: u32 = 8;
pub const PLUGIN_CAP_RETAIN: u32 = 9;
pub const PLUGIN_CAP_RELEASE: u32 = 10;
pub const PLUGIN_RELEASE_OUTPUT: u32 = 11;
pub const PLUGIN_FINALIZE: u32 = 12;

pub const HOST_CAP_RETAIN: u32 = 257;
pub const HOST_CAP_RELEASE: u32 = 258;
pub const HOST_CAP_OPEN: u32 = 259;
pub const HOST_CHANNEL_RECV: u32 = 260;
pub const HOST_CHANNEL_SEND: u32 = 261;
pub const HOST_CHANNEL_FINISH_REQUESTS: u32 = 262;
pub const HOST_CHANNEL_TERMINAL: u32 = 263;
pub const HOST_CHANNEL_CANCELLED: u32 = 264;
pub const HOST_EFFECT_BEGIN: u32 = 265;
pub const HOST_EFFECT_DEFER: u32 = 266;
pub const HOST_EFFECT_COMMIT: u32 = 267;
pub const HOST_EFFECT_ABORT: u32 = 268;
pub const HOST_PROVIDE: u32 = 269;
pub const HOST_RELEASE_OUTPUT: u32 = 270;

pub const CAP_KIND_FACTORY: u32 = 1;
pub const CAP_KIND_PREPARED: u32 = 2;
pub const CAP_KIND_INSTANCE: u32 = 3;
pub const CAP_KIND_SERVICE: u32 = 4;
pub const CAP_KIND_CALL_CHANNEL: u32 = 5;
pub const CAP_KIND_PROVIDER_CHANNEL: u32 = 6;
pub const CAP_KIND_EFFECT_TXN: u32 = 7;
pub const CAP_KIND_CLEANUP: u32 = 8;
pub const CAP_KIND_ACTIVATION: u32 = 9;

pub const RIGHT_RETAIN: u32 = 1 << 0;
pub const RIGHT_OPEN: u32 = 1 << 1;
pub const RIGHT_RECEIVE: u32 = 1 << 2;
pub const RIGHT_SEND: u32 = 1 << 3;
pub const RIGHT_FINISH: u32 = 1 << 4;
pub const RIGHT_MUTATE: u32 = 1 << 5;
pub const KNOWN_RIGHTS: u32 =
    RIGHT_RETAIN | RIGHT_OPEN | RIGHT_RECEIVE | RIGHT_SEND | RIGHT_FINISH | RIGHT_MUTATE;
