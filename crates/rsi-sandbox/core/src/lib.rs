//! Platform-neutral sandbox planning contracts.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta_contract::LocalContract;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use thiserror::Error;

mod workspace;
pub use workspace::{SandboxGeneration, WorkspaceReadRequest, WorkspaceReadScope};

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
    /// Requests read-only file effects; the stamp identifies the native boundary.
    ReadOnly,
    /// Requests write access to one workspace; the stamp identifies the boundary.
    WorkspaceWrite,
    /// Explicit bypass without confinement.
    DangerFullAccess,
}

/// Process-local I/O intent; never an enforcement stamp or durable field.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProcessStdio {
    /// Ordinary pipe-based execution.
    #[default]
    Pipes,
    /// A controlling terminal established before the confined wrapper starts.
    Pty,
}

/// Explicit process request before sandbox planning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessRequest<P = PathBuf> {
    /// Exact pipe or controlling-terminal intent.
    pub stdio: ProcessStdio,
    /// Requested policy.
    pub mode: SandboxMode,
    /// Absolute executable path, or the execution owner's resolved-program value.
    pub program: P,
    /// Exact argv excluding `argv[0]`.
    pub arguments: Vec<String>,
    /// Canonical working directory candidate.
    pub cwd: PathBuf,
    /// Canonical workspace candidate.
    pub workspace: PathBuf,
}

impl<P> ProcessRequest<P> {
    /// Moves the resolved program representation while preserving the exact invocation policy.
    pub fn map_program<Q>(self, map: impl FnOnce(P) -> Q) -> ProcessRequest<Q> {
        ProcessRequest {
            stdio: self.stdio,
            mode: self.mode,
            program: map(self.program),
            arguments: self.arguments,
            cwd: self.cwd,
            workspace: self.workspace,
        }
    }
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
    /// Native Low-integrity write-restricted token and Windows ACL grants.
    WindowsAcl,
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
    /// Windows ACL write restriction with existing ambient Low-object grants.
    WindowsAclReadOnly,
    /// Windows ACL restriction plus workspace and private-directory grants.
    WindowsAclWorkspaceWrite,
}

/// Durable scratch-directory evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxScratch {
    /// The child sees the host scratch namespace.
    Host,
    /// The child sees a private tmpfs mounted at `/tmp`.
    PrivateTmp,
    /// An owned directory selected through TEMP/TMP, without a private namespace.
    PrivateDirectory,
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
        let expected = match (&self.backend, self.requested) {
            (SandboxBackend::WindowsAcl, SandboxMode::ReadOnly) => {
                SandboxFileSystem::WindowsAclReadOnly
            }
            (SandboxBackend::WindowsAcl, SandboxMode::WorkspaceWrite) => {
                SandboxFileSystem::WindowsAclWorkspaceWrite
            }
            (_, SandboxMode::ReadOnly) => SandboxFileSystem::ReadOnly,
            (_, SandboxMode::WorkspaceWrite) => SandboxFileSystem::WorkspaceWrite,
            (_, SandboxMode::DangerFullAccess) => SandboxFileSystem::Unconfined,
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
                (self.requested != SandboxMode::DangerFullAccess
                    && self.scratch == SandboxScratch::PrivateTmp
                    && self.network == SandboxNetwork::Host)
                    || (self.requested == SandboxMode::ReadOnly
                        && self.scratch == SandboxScratch::Host
                        && self.network == SandboxNetwork::Isolated)
                    || (self.requested == SandboxMode::ReadOnly
                        && self.scratch == SandboxScratch::PrivateTmp
                        && self.network == SandboxNetwork::Isolated)
            }
            SandboxBackend::Landlock { .. } => {
                self.requested != SandboxMode::DangerFullAccess
                    && self.scratch == SandboxScratch::Host
                    && self.network == SandboxNetwork::Host
            }
            SandboxBackend::WindowsAcl => {
                self.network == SandboxNetwork::Host
                    && self.scratch == SandboxScratch::PrivateDirectory
                    && self.requested != SandboxMode::DangerFullAccess
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
    rsi_workspace_path::is_normalized_absolute_path(path)
}

/// Opaque native resources that must outlive execution of a confined plan.
#[derive(Clone, Debug)]
pub struct ProcessPlanOwner {
    retained: std::sync::Arc<dyn fmt::Debug + Send + Sync>,
    #[cfg(windows)]
    launch: Option<std::sync::Arc<WindowsLaunch>>,
}
#[cfg(windows)]
#[derive(Debug)]
struct WindowsLaunch {
    token: std::os::windows::io::OwnedHandle,
    temporary: PathBuf,
}
impl ProcessPlanOwner {
    /// Retains provider-owned resources without exposing native operations in the contract.
    pub fn new(owner: impl fmt::Debug + Send + Sync + 'static) -> Self {
        Self {
            retained: std::sync::Arc::new(owner),
            #[cfg(windows)]
            launch: None,
        }
    }
    /// Adds a lifetime pin without hiding native launch resources.
    #[must_use]
    pub fn retain(self, owner: impl fmt::Debug + Send + Sync + 'static) -> Self {
        Self {
            retained: std::sync::Arc::new((self.retained, owner)),
            #[cfg(windows)]
            launch: self.launch,
        }
    }
    /// Retains an issuer-prepared primary token, grants and private scratch path.
    #[cfg(windows)]
    pub fn windows(
        owner: impl fmt::Debug + Send + Sync + 'static,
        token: std::os::windows::io::OwnedHandle,
        temporary: PathBuf,
    ) -> Self {
        Self {
            retained: std::sync::Arc::new(owner),
            launch: Some(std::sync::Arc::new(WindowsLaunch { token, temporary })),
        }
    }
    /// Borrows the exact primary token while this plan remains retained.
    #[cfg(windows)]
    pub fn windows_token(&self) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
        use std::os::windows::io::AsHandle;
        self.launch.as_ref().map(|launch| launch.token.as_handle())
    }
}
impl PartialEq for ProcessPlanOwner {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.retained, &other.retained)
    }
}
impl Eq for ProcessPlanOwner {}

/// Exact process invocation after sandbox wrapping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfinedProcess {
    /// Retain through child settlement; never serialize native resource ownership.
    pub owner: Option<ProcessPlanOwner>,
    /// Exact I/O plan accepted by the sandbox provider.
    pub stdio: ProcessStdio,
    /// Executable to spawn.
    pub program: PathBuf,
    /// Exact wrapper or target arguments.
    pub arguments: Vec<OsString>,
    /// Working directory for the host spawn call.
    pub cwd: PathBuf,
    /// Truthful selected enforcement.
    pub stamp: EnforcementStamp,
}

impl ConfinedProcess {
    /// Applies native Sandbox scratch selection; returns whether it was selected.
    #[cfg(windows)]
    pub fn prepare_environment(&self, environment: &mut Vec<(OsString, OsString)>) -> bool {
        let Some(temporary) = self
            .owner
            .as_ref()
            .and_then(|owner| owner.launch.as_ref())
            .map(|launch| &launch.temporary)
        else {
            return false;
        };
        environment.retain(|(name, _)| {
            !name.eq_ignore_ascii_case("TEMP") && !name.eq_ignore_ascii_case("TMP")
        });
        environment.extend([
            ("TEMP".into(), temporary.as_os_str().to_owned()),
            ("TMP".into(), temporary.as_os_str().to_owned()),
        ]);
        true
    }

    /// Other backends express scratch selection in their exact wrapper arguments.
    #[cfg(not(windows))]
    #[allow(
        clippy::unused_self,
        reason = "The common preparation API has no environment edits on this platform."
    )]
    pub fn prepare_environment(&self, _: &mut Vec<(OsString, OsString)>) -> bool {
        false
    }
}

/// Closed sandbox failure taxonomy.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SandboxError {
    /// All bounded process-plan workers are occupied.
    #[error("sandbox preparation capacity is exhausted")]
    Capacity,
    /// A process-plan worker did not complete normally.
    #[error("sandbox preparation worker failed")]
    Preparation,
    /// Malformed, missing, or out-of-bounds path/argv input.
    #[error("invalid sandbox request: {0}")]
    InvalidInput(String),
    /// No verified backend can enforce this restricted mode.
    #[error("sandbox mode `{0:?}` has no available enforcement backend")]
    Unsupported(SandboxMode),
    /// Feature probe failed unexpectedly.
    #[error("sandbox probe failed: {0}")]
    Probe(String),
    /// Native token or ACL preparation failed with an operating-system diagnostic.
    #[error("native sandbox preparation failed: {0}")]
    Native(String),
}

/// Sandbox result.
pub type Result<T> = std::result::Result<T, SandboxError>;

/// Process-plan confinement service.
#[async_trait]
pub trait Sandbox: fmt::Debug + Send + Sync + 'static {
    /// Builds a narrow PID/net/mount-isolated, pipe-only runtime with private scratch.
    /// Explicit mounts and cgroup supervisor are issuer-selected; unsupported providers fail closed.
    async fn confine_isolated(&self, _request: IsolatedProcessRequest) -> Result<ConfinedProcess> {
        Err(SandboxError::Unsupported(SandboxMode::ReadOnly))
    }
    /// Verifies effective limits of a launched isolated scope before untrusted work starts.
    /// The issuer supplies the same immutable request and its private user-manager directory.
    async fn verify_isolated_limits(
        &self,
        _request: &IsolatedProcessRequest,
        _user_runtime: &Path,
    ) -> Result<()> {
        Err(SandboxError::Unsupported(SandboxMode::ReadOnly))
    }
    /// Confines a trusted fixed source collector without masking read-only host scratch.
    /// Providers that cannot guarantee this view must reject it.
    async fn confine_source_reader(&self, request: ProcessRequest) -> Result<ConfinedProcess> {
        Err(SandboxError::Unsupported(request.mode))
    }
    /// Issues a workspace-only read scope using the exact requested policy.
    async fn workspace_read(&self, request: WorkspaceReadRequest) -> Result<WorkspaceReadScope>;
    /// Validates and wraps one process request.
    async fn confine(&self, request: ProcessRequest) -> Result<ConfinedProcess>;
}

/// Explicit read-only mount; no ambient root bind is implied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadOnlyMount {
    /// Absolute operator-selected resource, checked without following a final symlink at planning.
    /// The issuer keeps this pathname and its ancestors stable through process settlement.
    pub source: PathBuf,
    /// Absolute location inside the isolated root.
    pub destination: PathBuf,
}
/// Exact resource-limited isolated runtime intent, distinct from source collection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IsolatedProcessRequest {
    /// Host-side immutable supervisor executable.
    pub supervisor: PathBuf,
    /// Unique owner-created transient user service identity: `rsi-` plus bounded ASCII alphanumeric/dashes.
    pub unit: String,
    /// Fixed executable inside the restricted mount view.
    pub program: PathBuf,
    /// Bounded literal arguments; never shell syntax.
    pub arguments: Vec<String>,
    /// The issuer's private runtime directory, retained as the stamp coordinate.
    pub workspace: PathBuf,
    /// Exact read-only runtime resources, at most sixteen.
    pub mounts: Vec<ReadOnlyMount>,
}
impl IsolatedProcessRequest {
    /// Checks the whole intent before selecting native enforcement.
    pub fn validate(&self) -> Result<()> {
        if !is_lexically_normal_absolute(&self.supervisor)
            || !is_lexically_normal_absolute(&self.program)
            || !is_lexically_normal_absolute(&self.workspace)
            || matches!(self.workspace.to_str(), Some("/" | "/tmp"))
            || !self.unit.starts_with("rsi-")
            || self.unit.len() <= 4
            || self.unit.len() > 80
            || !self
                .unit
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || self.arguments.len() > 32
            || self
                .arguments
                .iter()
                .any(|a| a.len() > 8192 || a.contains('\0'))
            || self.mounts.is_empty()
            || self.mounts.len() > 16
        {
            return Err(SandboxError::InvalidInput(
                "invalid isolated runtime intent".into(),
            ));
        }
        let mut destinations = std::collections::BTreeSet::new();
        for mount in &self.mounts {
            if !is_lexically_normal_absolute(&mount.source)
                || !is_lexically_normal_absolute(&mount.destination)
                || mount.source == Path::new("/")
                || mount.destination == Path::new("/")
                || ["/tmp", "/proc", "/dev"]
                    .iter()
                    .any(|root| mount.destination.starts_with(root))
                || !destinations.insert(&mount.destination)
            {
                return Err(SandboxError::InvalidInput(
                    "invalid isolated read-only mount".into(),
                ));
            }
        }
        if !self
            .mounts
            .iter()
            .any(|m| self.program.starts_with(&m.destination))
        {
            return Err(SandboxError::InvalidInput(
                "isolated executable has no explicit mount".into(),
            ));
        }
        Ok(())
    }
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
    fn windows_acl_evidence_does_not_claim_mount_or_network_isolation() {
        for (mode, filesystem) in [
            (SandboxMode::ReadOnly, SandboxFileSystem::WindowsAclReadOnly),
            (
                SandboxMode::WorkspaceWrite,
                SandboxFileSystem::WindowsAclWorkspaceWrite,
            ),
        ] {
            let valid = stamp(
                mode,
                SandboxBackend::WindowsAcl,
                filesystem,
                SandboxScratch::PrivateDirectory,
                SandboxNetwork::Host,
            );
            let encoded = serde_json::to_value(&valid).unwrap();
            assert_eq!(
                serde_json::from_value::<EnforcementStamp>(encoded.clone()).unwrap(),
                valid
            );
            for (field, value) in [
                ("filesystem", "read_only"),
                ("filesystem", "workspace_write"),
                ("scratch", "host"),
                ("scratch", "private_tmp"),
                ("network", "isolated"),
                ("requested", "danger-full-access"),
            ] {
                let mut invalid = encoded.clone();
                invalid[field] = value.into();
                assert!(
                    serde_json::from_value::<EnforcementStamp>(invalid).is_err(),
                    "{mode:?}: {field}={value}"
                );
            }
        }
    }

    #[test]
    #[cfg(windows)]
    fn lifetime_pins_preserve_the_native_handle_and_exact_private_environment() {
        use std::{
            os::windows::io::AsRawHandle,
            sync::{
                Arc,
                atomic::{AtomicUsize, Ordering},
            },
        };
        #[derive(Debug)]
        struct Released(Arc<AtomicUsize>);
        impl Drop for Released {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        // Handle ownership only: no native token creation or host policy edits.
        let handle: std::os::windows::io::OwnedHandle =
            std::fs::File::open(std::env::current_exe().unwrap())
                .unwrap()
                .into();
        let raw = handle.as_raw_handle();
        let released = Arc::new(AtomicUsize::new(0));
        let owner = ProcessPlanOwner::windows(
            Released(released.clone()),
            handle,
            r"C:\private-scratch".into(),
        );
        let original = owner.clone();
        let pinned = owner.retain(Released(released.clone()));
        assert_eq!(pinned.windows_token().unwrap().as_raw_handle(), raw);
        let mut plan = ConfinedProcess {
            owner: Some(pinned),
            stdio: ProcessStdio::Pipes,
            program: r"C:\program.exe".into(),
            cwd: r"C:\workspace".into(),
            arguments: vec![],
            stamp: stamp(
                SandboxMode::ReadOnly,
                SandboxBackend::WindowsAcl,
                SandboxFileSystem::WindowsAclReadOnly,
                SandboxScratch::PrivateDirectory,
                SandboxNetwork::Host,
            ),
        };
        let mut environment = vec![
            ("Temp".into(), "old".into()),
            ("tMp".into(), "old".into()),
            ("KEEP".into(), "value".into()),
        ];
        assert!(plan.prepare_environment(&mut environment));
        assert_eq!(
            environment,
            vec![
                ("KEEP".into(), "value".into()),
                ("TEMP".into(), r"C:\private-scratch".into()),
                ("TMP".into(), r"C:\private-scratch".into())
            ]
        );
        drop(original);
        assert_eq!(released.load(Ordering::SeqCst), 0);
        drop(plan.owner.take());
        assert_eq!(released.load(Ordering::SeqCst), 2);
        let frozen = environment.clone();
        assert!(!plan.prepare_environment(&mut environment));
        assert_eq!(environment, frozen);
    }

    #[test]
    fn source_reader_evidence_requires_read_only_bubblewrap_and_isolated_network() {
        let mut source = stamp(
            SandboxMode::ReadOnly,
            SandboxBackend::Bubblewrap {
                sha256: "a".repeat(64),
            },
            SandboxFileSystem::ReadOnly,
            SandboxScratch::Host,
            SandboxNetwork::Isolated,
        );
        source.validate().unwrap();
        source.network = SandboxNetwork::Host;
        assert!(source.validate().is_err());
        source.network = SandboxNetwork::Isolated;
        source.requested = SandboxMode::WorkspaceWrite;
        source.filesystem = SandboxFileSystem::WorkspaceWrite;
        assert!(source.validate().is_err());
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

#[cfg(test)]
mod isolated_request_tests {
    use super::*;
    #[test]
    fn isolated_intent_cannot_shadow_private_mounts_or_execute_unmounted_programs() {
        let request = IsolatedProcessRequest {
            supervisor: "/usr/bin/systemd-run".into(),
            unit: "rsi-browser-test".into(),
            program: "/runtime/node".into(),
            arguments: vec![],
            workspace: "/owned/scratch".into(),
            mounts: vec![ReadOnlyMount {
                source: "/installed/node".into(),
                destination: "/runtime/node".into(),
            }],
        };
        request.validate().unwrap();
        for destination in ["/runtime/node-extra", "/runtime/node2"] {
            let mut bad = request.clone();
            bad.mounts[0].destination = destination.into();
            assert!(
                bad.validate().is_err(),
                "path prefixes are not mount containment"
            );
        }
        let mut shortest = request.clone();
        shortest.unit = "rsi-x".into();
        shortest.validate().unwrap();
        let mut other_consumer = request.clone();
        other_consumer.unit = "rsi-source-index-test".into();
        other_consumer.validate().unwrap();
        for unit in [
            "",
            "rsi-",
            "other-owner",
            "rsi-../scope",
            "rsi-scope.service",
            "rsi-\0scope",
        ] {
            let mut bad = request.clone();
            bad.unit = unit.into();
            assert!(bad.validate().is_err());
        }
        let mut bad = request.clone();
        bad.unit = format!("rsi-{}", "x".repeat(77));
        assert!(bad.validate().is_err());
        for destination in [
            "/",
            "/tmp",
            "/dev",
            "/proc",
            "/tmp/cache",
            "/proc/self",
            "/dev/shm",
        ] {
            let mut bad = request.clone();
            bad.mounts[0].destination = destination.into();
            bad.program = PathBuf::from(destination).join("node");
            assert!(bad.validate().is_err());
        }
        let mut bad = request.clone();
        bad.program = "/usr/bin/sh".into();
        assert!(bad.validate().is_err());
        let mut bad = request;
        bad.arguments = vec!["x".into(); 33];
        assert!(bad.validate().is_err());
    }
}
