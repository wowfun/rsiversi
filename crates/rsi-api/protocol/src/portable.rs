//! Closed framing for an explicitly granted Portable API client capability.
use crate::{ConnectionDescription, OperationCatalog, OperationId};
use serde::{Deserialize, Serialize};

/// Exact Meta Portable contract identifier.
pub const CONTRACT: &str = "rsi.api.portable";
/// Contract revision, independent of domain API versions.
pub const VERSION: u32 = 1;
/// Maximum encoded control header, excluding its one-byte tag.
pub const MAXIMUM_HEADER_BYTES: usize = 1024;
/// Maximum bytes in one payload fragment, excluding its tag and offset.
pub const MAXIMUM_FRAGMENT_BYTES: usize = 64 * 1024;
/// Maximum complete negotiated connection document.
pub const MAXIMUM_DESCRIPTION_BYTES: usize = 1024 * 1024;
/// Control-header tag. The remaining bytes encode exactly one `Header`.
pub const HEADER_TAG: u8 = 0;
/// Payload-fragment tag, followed by a little-endian u32 offset and raw bytes.
pub const FRAGMENT_TAG: u8 = 1;

/// One immutable connection description and its explicitly exported operations.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Description {
    /// Existing authenticated or local connection identity, never caller-provided authority.
    pub connection: ConnectionDescription,
    /// Exact selected owning operation contracts.
    pub operations: OperationCatalog,
}

/// A bounded control frame. Each declared payload follows in contiguous fragments.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Header {
    /// Requests the connection description, followed immediately by request EOF.
    Describe {},
    /// The following JSON document is a `Description`.
    Description {
        /// Complete document length, at most one MiB.
        bytes: usize,
    },
    /// Calls one selected operation; metadata and authority come from the exporter.
    Call {
        /// Exact domain-owned operation identity.
        operation: OperationId,
        /// Complete request body length, bounded by the registered operation.
        bytes: usize,
    },
    /// One finite response, followed by a clean Meta terminal result.
    Reply {
        /// JSON prefix length in the combined payload.
        json: usize,
        /// Optional raw binary suffix length; `Some(0)` preserves an empty binary part.
        binary: Option<usize>,
    },
    /// Starts an API subscription; only Item, End or Error can follow.
    Stream {},
    /// One complete subscription message, with a combined JSON/binary payload.
    Item {
        /// JSON prefix length.
        json: usize,
        /// Optional raw binary suffix length.
        binary: Option<usize>,
    },
    /// Explicit clean subscription end, followed by a clean Meta terminal result.
    End {},
    /// Terminal API failure. Diagnostics are closed codes; domain errors retain JSON bytes.
    Error {
        /// API-level failure category.
        code: ErrorCode,
        /// Domain JSON length, present only for Domain errors.
        domain: Option<usize>,
    },
}
/// Redacted API error vocabulary independent of native/HTTP diagnostic text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The mutation may have executed; the caller must use domain receipts.
    OutcomeUnknown,
    /// Caller authority has been revoked or rejected.
    Unauthorized,
    /// Provider implementation failure; diagnostic bytes are not forwarded.
    Backend,
    /// Bounded domain-owned JSON follows.
    Domain,
    /// Invalid request or response shape.
    Invalid,
    /// A bounded API lane or byte pool is full.
    Capacity,
    /// The supplying generation is shutting down.
    ShuttingDown,
    /// The selected operation is unavailable.
    Unavailable,
}
