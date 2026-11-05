//! Bounded process extensions with independent request and callback lanes.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod connection;
mod factory;
mod wire;
pub use connection::{Connection, Handler, HandlerRegistration, LaunchResources, RejectRequests};
pub use factory::{ExtensionContract, ExtensionFactory, Launch};
use thiserror::Error;
pub use wire::{Frame, MAXIMUM_FRAME_BYTES, MAXIMUM_PENDING_REQUESTS, Message, Response};

/// A connection failure never grants callback replay authority.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum Error {
    /// Malformed, mismatched or unbounded external data.
    #[error("invalid extension protocol")]
    Protocol,
    /// Bounded admission is full.
    #[error("extension capacity exceeded")]
    Capacity,
    /// The exact generation is no longer connected.
    #[error("extension disconnected; operation outcome may be unknown")]
    Disconnected,
    /// The caller stopped waiting; the peer must still settle its request.
    #[error("extension request cancelled")]
    Cancelled,
    /// Bounded diagnostic returned by the peer.
    #[error("extension rejected request: {0}")]
    Remote(String),
}
/// Result from the extension owner.
pub type Result<T> = std::result::Result<T, Error>;
