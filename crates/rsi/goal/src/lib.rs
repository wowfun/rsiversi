//! Host-owned live Goal continuation control.

#![deny(unsafe_code)]
#![warn(missing_docs)]

mod protocol;
pub use protocol::*;
#[cfg(not(target_arch = "wasm32"))]
mod controller;
#[cfg(not(target_arch = "wasm32"))]
pub use controller::{GoalControllerFactory, GoalService};
