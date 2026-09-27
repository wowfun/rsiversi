//! Unix ACP stdio application plugin.
#![deny(unsafe_code)]
#[cfg(unix)]
mod application;
#[cfg(unix)]
pub use application::ApplicationFactory;
