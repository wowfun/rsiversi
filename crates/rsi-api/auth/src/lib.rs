//! Native device authentication and durable local administration plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod plugin;
mod registry;
pub use plugin::{DeviceAuthConfig, DeviceAuthFactory};
pub use registry::DeviceRegistry;
