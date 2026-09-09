//! Rust Worker application and renderer plugins for the Web coding workspace.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod application;
mod catalog;
mod details;
mod panes;
mod projection;
mod renderer;
pub use application::{WebApplication, WebApplicationContract, WebApplicationFactory};
#[cfg(target_arch = "wasm32")]
mod worker;
