//! Authenticated external-conversation adapters without endpoint launch authority.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
mod client;
mod plugin;
mod server;
mod validate;
mod wire;
pub use client::Client;
pub use plugin::{ClientFactory, EndpointFactory};
pub use server::Endpoint;

#[cfg(test)]
mod tests;
