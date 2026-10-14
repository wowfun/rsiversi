//! Native ACP stdio application plugin.
#![deny(unsafe_code)]
#[cfg(any(unix, windows))]
mod application;
#[cfg(any(unix, windows))]
pub use application::ApplicationFactory;
