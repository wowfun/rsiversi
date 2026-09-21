//! Optional ordinary read-only language-server addon.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod json;
mod owner;
mod plugin;
mod protocol;
mod source;
mod tools;
mod wire;
pub use owner::LanguageService;
pub use plugin::{LanguageContract, LanguageFactory};
pub use protocol::{
    Config, Location, Operation, Output, Position, Query, QueryResult, Range, byte_offset,
    position, relative,
};
pub use tools::{LanguageToolsFactory, output_declaration};
/// Finite failure categories; server text and stderr are never copied into errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    /// Malformed input or unsupported source coordinate.
    #[error("invalid language query or source")]
    Invalid,
    /// A configured limit was exceeded.
    #[error("language query exceeds its limit")]
    Limit,
    /// No admission slot is available.
    #[error("language provider is at capacity")]
    Capacity,
    /// Provider generation is retired.
    #[error("language provider retired")]
    Retired,
    /// Caller cancelled work.
    #[error("language query cancelled")]
    Cancelled,
    /// Operation exceeded its deadline.
    #[error("language query deadline exceeded")]
    Deadline,
    /// Malformed or uncertain wire state.
    #[error("language server protocol failed")]
    Protocol,
    /// A correlated server rejection; arbitrary message/data are omitted.
    #[error("language server rejected request (code {0})")]
    Server(i32),
    /// Server, filesystem or sandbox is unavailable.
    #[error("language server or source unavailable")]
    Unavailable,
    /// Selected server does not support this operation or document synchronization.
    #[error("language server capability unavailable")]
    Unsupported,
}
/// Language addon result.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests;
