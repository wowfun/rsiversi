//! Dedicated Worker adapter for the shared Rust GUI application.
#![deny(unsafe_code)]
#![warn(missing_docs)]

#[cfg(target_arch = "wasm32")]
mod assets;
#[cfg(target_arch = "wasm32")]
mod worker;
