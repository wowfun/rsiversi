//! Bounded renderer-neutral presentation data, without executable authority.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod view;
pub use view::*;
mod model;
pub use model::*;
pub mod portable;
mod renderer;
pub use renderer::*;
mod scope;
pub use scope::ExportScope;

/// Maximum encoded action input including its fields.
pub const MAXIMUM_INPUT_BYTES: usize = 64 * 1024;
/// Maximum encoded view or model snapshot including its envelope.
pub const MAXIMUM_VIEW_BYTES: usize = 128 * 1024;
/// Maximum flat standard-view elements.
pub const MAXIMUM_ELEMENTS: usize = 256;
/// Maximum actions or sources in a model.
pub const MAXIMUM_MODEL_REFERENCES: usize = 32;
/// Current presentation ABI version.
pub const PRESENTATION_ABI: u16 = 1;

/// Rejected external presentation data.
#[derive(Debug, thiserror::Error)]
#[error("invalid UI data: {0}")]
pub struct ProtocolError(pub String);
/// Bounded data validation result.
pub type Result<T> = std::result::Result<T, ProtocolError>;
