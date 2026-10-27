//! Native ACP stdio application plugin.
#![deny(unsafe_code)]
/// Public stdio application invocation.
pub const HELP: &str = "Usage: rsi [GLOBAL OPTIONS] acp\nServe ACP over stdin/stdout. This application accepts no additional options.\n";
#[cfg(any(unix, windows))]
mod application;
#[cfg(any(unix, windows))]
pub use application::ApplicationFactory;
