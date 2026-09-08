//! Domain-owned Session API endpoints and shared application client.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod client;
mod client_stream;
mod plugin;
mod server;
mod server_stream;
mod wire;

pub use client::SessionClient;
pub use plugin::{SessionApiFactory, SessionClientFactory};
pub use server::SessionApi;
