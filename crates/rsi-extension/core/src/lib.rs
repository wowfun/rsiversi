//! Bounded process extensions with independent request and callback lanes.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

pub mod body;
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
    #[error("extension disconnected")]
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

/// Dispatch evidence for one failed outbound request, independent of its cause.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RequestFailure {
    /// The request never entered the writer queue and cannot have executed.
    #[error("extension request was not dispatched: {0}")]
    NotDispatched(Error),
    /// The request entered the writer queue; only business evidence can settle it.
    #[error("extension request may have executed: {0}")]
    MayHaveExecuted(Error),
}

impl RequestFailure {
    /// Returns the cause without discarding this request's dispatch evidence.
    pub const fn cause(&self) -> &Error {
        match self {
            Self::NotDispatched(error) | Self::MayHaveExecuted(error) => error,
        }
    }
}
