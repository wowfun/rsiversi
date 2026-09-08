//! Domain-independent API registry and owned invocation supervision.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod connection;
mod invocation;
mod plugin;
mod registry;
mod stream;
pub use connection::{ConnectionApi, ConnectionApiFactory};
pub use plugin::ApiFactory;
pub use registry::ApiRegistry;
