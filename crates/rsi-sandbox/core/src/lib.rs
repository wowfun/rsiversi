//! Platform-neutral sandbox planning contracts.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta_contract::LocalContract;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use thiserror::Error;

/// Maximum argv items in one sandbox process plan.
pub const MAXIMUM_SANDBOX_ARGUMENTS: usize = 4_096;
/// Maximum total UTF-8 bytes in program, argv, and paths.
pub const MAXIMUM_SANDBOX_PLAN_BYTES: usize = 1024 * 1024;
/// Maximum bytes copied from one explicitly configured sandbox wrapper.
pub const MAXIMUM_SANDBOX_WRAPPER_BYTES: usize = 16 * 1024 * 1024;

/// Requested file-effect policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    /// Read-only host filesystem view.
    ReadOnly,
    /// Read-only host view with one writable canonical workspace.
    WorkspaceWrite,
    /// Explicit bypass without confinement.
    DangerFullAccess,
}

/// Explicit process request before sandbox planning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessRequest {
    /// Requested policy.
    pub mode: SandboxMode,
    /// Absolute executable path.
    pub program: PathBuf,
    /// Exact argv excluding `argv[0]`.
    pub arguments: Vec<String>,
    /// Canonical working directory candidate.
    pub cwd: PathBuf,
    /// Canonical workspace candidate.
    pub workspace: PathBuf,
}

/// Actually selected enforcement backend.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SandboxBackend {
    /// bubblewrap wrapper.
    Bubblewrap {
        /// Exact verified wrapper path.
        path: PathBuf,
    },
    /// Explicit Landlock runner wrapper.
    Landlock {
        /// Exact verified helper path.
        path: PathBuf,
    },
    /// Explicit danger-full-access bypass.
    Unconfined,
}

/// Durable-safe truth about one selected process plan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnforcementStamp {
    /// Requested mode.
    pub requested: SandboxMode,
    /// Actually selected backend.
    pub backend: SandboxBackend,
    /// Canonical workspace used by the plan.
    pub workspace: PathBuf,
    /// Whether the plan grants workspace writes.
    pub workspace_writable: bool,
    /// Network enforcement is outside the current mode vocabulary.
    pub network_restricted: bool,
}

/// Exact process invocation after sandbox wrapping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfinedProcess {
    /// Executable to spawn.
    pub program: PathBuf,
    /// Exact wrapper or target arguments.
    pub arguments: Vec<OsString>,
    /// Working directory for the host spawn call.
    pub cwd: PathBuf,
    /// Truthful selected enforcement.
    pub stamp: EnforcementStamp,
}

/// Closed sandbox failure taxonomy.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SandboxError {
    /// Malformed, missing, or out-of-bounds path/argv input.
    #[error("invalid sandbox request: {0}")]
    InvalidInput(String),
    /// No verified backend can enforce this restricted mode.
    #[error("sandbox mode `{0:?}` has no available enforcement backend")]
    Unsupported(SandboxMode),
    /// Feature probe failed unexpectedly.
    #[error("sandbox probe failed: {0}")]
    Probe(String),
}

/// Sandbox result.
pub type Result<T> = std::result::Result<T, SandboxError>;

/// Process-plan confinement service.
#[async_trait]
pub trait Sandbox: fmt::Debug + Send + Sync + 'static {
    /// Validates and wraps one process request.
    async fn confine(&self, request: ProcessRequest) -> Result<ConfinedProcess>;
}

/// Nominal Local contract for [`Sandbox`].
#[derive(Debug)]
pub struct SandboxContract;

impl LocalContract for SandboxContract {
    const KEY: &'static str = "rsi.sandbox";
    type Service = dyn Sandbox;
}
