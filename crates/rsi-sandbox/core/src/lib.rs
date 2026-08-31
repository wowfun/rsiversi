//! Platform-neutral sandbox planning contracts.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta_contract::LocalContract;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fmt;
use std::path::{Component, Path, PathBuf};
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
        /// SHA-256 of the exact staged wrapper bytes.
        sha256: String,
    },
    /// Explicit Landlock runner wrapper.
    Landlock {
        /// SHA-256 of the exact staged runner bytes.
        sha256: String,
    },
    /// Explicit danger-full-access bypass.
    Unconfined,
}

/// Durable filesystem-write enforcement evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxFileSystem {
    /// No filesystem-write confinement.
    Unconfined,
    /// Host paths are visible read-only.
    ReadOnly,
    /// Host paths are read-only except for the canonical workspace.
    WorkspaceWrite,
}

/// Durable scratch-directory evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxScratch {
    /// The child sees the host scratch namespace.
    Host,
    /// The child sees a private tmpfs mounted at `/tmp`.
    PrivateTmp,
}

/// Durable network enforcement evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxNetwork {
    /// The child retains host network access.
    Host,
    /// The child is isolated from host networking.
    Isolated,
}

/// Durable-safe truth about one selected process plan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnforcementStamp {
    /// Requested mode.
    pub requested: SandboxMode,
    /// Actually selected backend.
    pub backend: SandboxBackend,
    /// Canonical workspace used by the plan.
    pub workspace: PathBuf,
    /// Filesystem-write evidence.
    pub filesystem: SandboxFileSystem,
    /// Scratch-directory evidence.
    pub scratch: SandboxScratch,
    /// Network evidence.
    pub network: SandboxNetwork,
}

impl<'de> Deserialize<'de> for EnforcementStamp {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireStamp {
            requested: SandboxMode,
            backend: SandboxBackend,
            workspace: PathBuf,
            filesystem: SandboxFileSystem,
            scratch: SandboxScratch,
            network: SandboxNetwork,
        }

        let wire = WireStamp::deserialize(deserializer)?;
        let stamp = Self {
            requested: wire.requested,
            backend: wire.backend,
            workspace: wire.workspace,
            filesystem: wire.filesystem,
            scratch: wire.scratch,
            network: wire.network,
        };
        stamp
            .validate()
            .map(|()| stamp)
            .map_err(serde::de::Error::custom)
    }
}

impl EnforcementStamp {
    /// Validates durable evidence fields independently of ephemeral process paths.
    pub fn validate(&self) -> Result<()> {
        if !is_lexically_normal_absolute(&self.workspace) {
            return Err(SandboxError::InvalidInput(
                "sandbox workspace must be a lexically normalized absolute path".into(),
            ));
        }
        if self.requested != SandboxMode::DangerFullAccess {
            if self.workspace == Path::new("/") {
                return Err(SandboxError::InvalidInput(
                    "restricted workspace cannot be the filesystem root".into(),
                ));
            }
            if self.workspace == Path::new("/tmp") {
                return Err(SandboxError::InvalidInput(
                    "restricted workspace cannot be exactly /tmp".into(),
                ));
            }
        }
        if let SandboxBackend::Bubblewrap { sha256 } | SandboxBackend::Landlock { sha256 } =
            &self.backend
            && (sha256.len() != 64
                || !sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')))
        {
            return Err(SandboxError::InvalidInput(
                "sandbox backend digest must be lowercase SHA-256 hex".into(),
            ));
        }
        let expected = match self.requested {
            SandboxMode::ReadOnly => SandboxFileSystem::ReadOnly,
            SandboxMode::WorkspaceWrite => SandboxFileSystem::WorkspaceWrite,
            SandboxMode::DangerFullAccess => SandboxFileSystem::Unconfined,
        };
        if self.filesystem != expected {
            return Err(SandboxError::InvalidInput(
                "sandbox filesystem evidence disagrees with the requested mode".into(),
            ));
        }
        let semantics_are_valid = match &self.backend {
            SandboxBackend::Unconfined => {
                self.requested == SandboxMode::DangerFullAccess
                    && self.scratch == SandboxScratch::Host
                    && self.network == SandboxNetwork::Host
            }
            SandboxBackend::Bubblewrap { .. } => {
                self.requested != SandboxMode::DangerFullAccess
                    && self.scratch == SandboxScratch::PrivateTmp
                    && self.network == SandboxNetwork::Host
            }
            SandboxBackend::Landlock { .. } => {
                self.requested != SandboxMode::DangerFullAccess
                    && self.scratch == SandboxScratch::Host
                    && self.network == SandboxNetwork::Host
            }
        };
        if !semantics_are_valid {
            return Err(SandboxError::InvalidInput(
                "sandbox backend, scratch, or network evidence is contradictory or unsupported"
                    .into(),
            ));
        }
        Ok(())
    }
}

fn is_lexically_normal_absolute(path: &Path) -> bool {
    if !path.is_absolute() || path.as_os_str().is_empty() {
        return false;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir | Component::ParentDir => return false,
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized.as_os_str() == path.as_os_str()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(
        requested: SandboxMode,
        backend: SandboxBackend,
        filesystem: SandboxFileSystem,
        scratch: SandboxScratch,
        network: SandboxNetwork,
    ) -> EnforcementStamp {
        EnforcementStamp {
            requested,
            backend,
            workspace: PathBuf::from("/workspace"),
            filesystem,
            scratch,
            network,
        }
    }

    #[test]
    fn enforcement_stamps_reject_contradictory_backend_semantics() {
        let digest = "0".repeat(64);
        for invalid in [
            stamp(
                SandboxMode::ReadOnly,
                SandboxBackend::Unconfined,
                SandboxFileSystem::ReadOnly,
                SandboxScratch::Host,
                SandboxNetwork::Host,
            ),
            stamp(
                SandboxMode::ReadOnly,
                SandboxBackend::Landlock {
                    sha256: digest.clone(),
                },
                SandboxFileSystem::ReadOnly,
                SandboxScratch::PrivateTmp,
                SandboxNetwork::Host,
            ),
            stamp(
                SandboxMode::WorkspaceWrite,
                SandboxBackend::Bubblewrap {
                    sha256: digest.clone(),
                },
                SandboxFileSystem::WorkspaceWrite,
                SandboxScratch::PrivateTmp,
                SandboxNetwork::Isolated,
            ),
            stamp(
                SandboxMode::DangerFullAccess,
                SandboxBackend::Unconfined,
                SandboxFileSystem::Unconfined,
                SandboxScratch::PrivateTmp,
                SandboxNetwork::Host,
            ),
        ] {
            assert!(invalid.validate().is_err(), "accepted {invalid:?}");
        }

        assert!(
            stamp(
                SandboxMode::ReadOnly,
                SandboxBackend::Bubblewrap { sha256: digest },
                SandboxFileSystem::ReadOnly,
                SandboxScratch::PrivateTmp,
                SandboxNetwork::Host,
            )
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn restricted_stamps_reject_unsafe_workspace_roots_for_every_backend() {
        let digest = "0".repeat(64);
        for workspace in [PathBuf::from("/"), PathBuf::from("/tmp")] {
            for (backend, scratch) in [
                (
                    SandboxBackend::Bubblewrap {
                        sha256: digest.clone(),
                    },
                    SandboxScratch::PrivateTmp,
                ),
                (
                    SandboxBackend::Landlock {
                        sha256: digest.clone(),
                    },
                    SandboxScratch::Host,
                ),
            ] {
                let stamp = EnforcementStamp {
                    requested: SandboxMode::WorkspaceWrite,
                    backend,
                    workspace: workspace.clone(),
                    filesystem: SandboxFileSystem::WorkspaceWrite,
                    scratch,
                    network: SandboxNetwork::Host,
                };
                assert!(stamp.validate().is_err(), "accepted {stamp:?}");
            }
        }
    }
}
