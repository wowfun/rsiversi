//! Ordinary application entry and scoped surface composition.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

pub mod arguments;
mod error;
mod profile;
mod shell;
pub use error::RsiError;
pub use profile::ScopedProfile;
pub use shell::{Shell, ShellContract, ShellFactory, Surface};

use futures_util::future::BoxFuture;
use std::sync::Arc;

/// Prepared application entry; its plugin owns work and final cleanup.
pub trait ApplicationRun: std::fmt::Debug + Send + Sync + 'static {
    /// Runs this generation once and returns its process/product exit status.
    fn run(self: Arc<Self>) -> BoxFuture<'static, Result<u8>>;
}

/// Nominal entry capability discovered after an Application Profile becomes active.
#[derive(Debug)]
pub struct ApplicationRunContract;
impl rsi_meta::LocalContract for ApplicationRunContract {
    const KEY: &'static str = "rsi.application.run";
    type Service = dyn ApplicationRun;
}

/// Failure owned by application composition or execution.
#[derive(Debug, thiserror::Error)]
pub enum ApplicationError {
    /// A child Profile failed to prepare, activate or reload.
    #[error(transparent)]
    Profile(#[from] rsi_host::HostError),
    /// A real child scope could not be created.
    #[error(transparent)]
    Scope(#[from] rsi_meta_scope::ScopeError),
    /// All bounded surface slots are occupied.
    #[error("application surface capacity is exhausted")]
    Capacity,
    /// The owning plugin generation has withdrawn.
    #[error("application is shutting down")]
    ShuttingDown,
    /// An application task failed without publishing its result.
    #[error("application task stopped without a result")]
    TaskStopped,
    /// The single entry point was already invoked for this generation.
    #[error("application entry was already started")]
    AlreadyStarted,
}

/// Application composition result.
pub type Result<T> = std::result::Result<T, ApplicationError>;
