use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::{
    CompositionLock, GraphRevision, GraphSnapshot, InstanceId, InstanceSnapshot, PackageId,
    ValidationReport,
};

/// Files and durable state owned by one composition host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompositionWorkspace {
    /// Private `SQLite` control-plane database.
    pub database_path: PathBuf,
    /// Root for verified package generations and loader artifacts.
    pub cache_root: PathBuf,
    /// Active composition manifest path.
    pub manifest_path: PathBuf,
    /// Active composition lock path.
    pub lock_path: PathBuf,
}

/// A composition candidate read independently from a live host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompositionProject {
    pub manifest_path: PathBuf,
    /// Optional pre-resolved lock; absence requests resolution.
    pub lock_path: Option<PathBuf>,
}

/// Caller-owned identity for one durable side-effecting operation.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OperationId(pub String);

impl OperationId {
    pub(crate) fn validate(&self) -> crate::Result<()> {
        const INTERNAL_PREFIXES: [&str; 3] = ["system:", "plugin-effect:", "plugin-rejection:"];
        if self.0.is_empty()
            || self.0.len() > 255
            || !self.0.bytes().all(|byte| byte.is_ascii_graphic())
            || INTERNAL_PREFIXES
                .iter()
                .any(|prefix| self.0.starts_with(prefix))
        {
            return Err(crate::HostError::OperationRejected {
                code: "invalid_operation_id".to_owned(),
                message: "operation id must contain 1 to 255 printable ASCII bytes without spaces and must not use an internal namespace".to_owned(),
                details: BTreeMap::new(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_operation_ids_cannot_enter_internal_namespaces() {
        for value in [
            "system:startup",
            "plugin-effect:claimed",
            "plugin-rejection:claimed",
        ] {
            let error = OperationId(value.to_owned()).validate().unwrap_err();
            assert!(matches!(
                error,
                crate::HostError::OperationRejected { ref code, .. }
                    if code == "invalid_operation_id"
            ));
        }
        OperationId("external-operation".to_owned())
            .validate()
            .unwrap();
    }

    #[test]
    fn caller_operation_ids_are_log_safe_and_byte_bounded() {
        for value in ["line\nbreak", "space separated", "unicode-操作"] {
            assert!(OperationId(value.to_owned()).validate().is_err());
        }
        assert!(OperationId("x".repeat(255)).validate().is_ok());
        assert!(OperationId("x".repeat(256)).validate().is_err());
    }
}

/// Stable identity of one canonical manifest/lock pair.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompositionDigest {
    /// Stable composition identity declared by the manifest.
    pub composition_id: String,
    /// Lowercase SHA-256 digest of canonical manifest bytes.
    pub manifest_sha256: String,
    /// Lowercase SHA-256 digest of canonical lock bytes.
    pub lock_sha256: String,
}

/// Request to atomically apply a candidate composition to a live host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplyRequest {
    /// Caller-owned idempotency identity.
    pub operation_id: OperationId,
    pub project: CompositionProject,
    /// Optional optimistic-concurrency precondition.
    pub expected_revision: Option<GraphRevision>,
}

/// Request to resolve and install a composition without starting a host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallRequest {
    /// Caller-owned idempotency identity.
    pub operation_id: OperationId,
    /// Workspace receiving durable state and cached generations.
    pub workspace: CompositionWorkspace,
    pub project: CompositionProject,
}

/// Result of applying a candidate composition to a live host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApplyResult {
    /// Candidate produced and committed a new active graph.
    Applied { snapshot: HostSnapshot },
    /// Candidate exactly matched the already active composition.
    Unchanged { snapshot: HostSnapshot },
    /// Process-fixed changes require a new host process.
    RestartRequired {
        /// Currently active composition, if any.
        current: Option<CompositionDigest>,
        /// Validated candidate composition.
        candidate: CompositionDigest,
        /// Packages whose process-fixed state prevents live replacement.
        packages: Vec<PackageId>,
    },
}

/// Result of resolving and installing a composition workspace.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InstallResult {
    /// Candidate state was newly installed.
    Installed { candidate: CompositionDigest },
    /// Workspace already contained the exact candidate state.
    Unchanged { candidate: CompositionDigest },
}

/// Result of resolving a composition lock.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LockResult {
    /// A new canonical lock was produced.
    Created { lock: CompositionLock },
    /// Existing lock already matched resolution.
    Unchanged { lock: CompositionLock },
}

/// Receipt returned after rotating host-issued tokens.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TokenRotation {
    /// New monotonic token generation.
    pub generation: u64,
}

/// Receipt proving an idempotent shutdown operation was accepted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShutdownReceipt {
    pub operation_id: OperationId,
}

/// Current committed host graph and durable event position.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HostSnapshot {
    /// Immutable active graph snapshot.
    pub graph: GraphSnapshot,
    /// Last durable host-event cursor.
    pub cursor: u64,
    /// Current token generation.
    pub token_generation: u64,
    /// Canonical active composition, if the host has one.
    pub active: Option<CompositionDigest>,
}

/// Operation that committed an active composition.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionChangeSource {
    /// Host startup or reopen.
    Open,
    /// Explicit host apply operation.
    Apply,
    /// Plugin-requested apply operation.
    PluginApply,
}

/// Durable host lifecycle and runtime event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostEvent {
    /// A composition graph was atomically committed.
    CompositionCommitted {
        /// Operation that initiated the commit.
        source: CompositionChangeSource,
        /// Stable composition identity.
        composition_id: String,
        /// Canonical manifest digest.
        manifest_sha256: String,
        /// Canonical lock digest.
        lock_sha256: String,
        /// Number of active instances after commit.
        active_instances: u32,
        /// Number of inactive instances after commit.
        inactive_instances: u32,
    },
    /// A mounted runtime instance entered a faulted state.
    RuntimeFaulted {
        /// Faulted instance identity.
        instance_id: InstanceId,
        /// Bounded fault summary.
        reason: String,
    },
    /// Host began its terminal shutdown sequence.
    HostShuttingDown,
    /// Forward-compatible event not understood by this binary.
    Unknown {
        /// Stable external event discriminator.
        event_type: String,
        /// Preserved event fields excluding the discriminator.
        payload: serde_json::Map<String, serde_json::Value>,
    },
}

/// One durable host event with ordering and graph correlation metadata.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HostEventRecord {
    /// Monotonic durable event cursor.
    pub cursor: u64,
    /// Caller operation that caused the event, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<OperationId>,
    /// Graph revision in effect when the event was committed.
    pub graph_revision: GraphRevision,
    pub event: HostEvent,
}

/// Ordered page of durable host events.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventPage {
    /// Records ordered by increasing cursor.
    pub events: Vec<HostEventRecord>,
}

/// Descriptor-oriented inspect result for one mounted plugin instance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginInspection {
    /// Current mounted instance snapshot.
    pub instance: InstanceSnapshot,
    /// Whether changing this package requires process restart.
    pub process_fixed: bool,
    /// Declared plugin capability names.
    pub capabilities: Vec<String>,
    /// Verified package-relative configuration schema path, when declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema_path: Option<PathBuf>,
    /// Parsed configuration schema, when declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<serde_json::Value>,
}

impl From<crate::protocol::PluginInspection> for PluginInspection {
    fn from(value: crate::protocol::PluginInspection) -> Self {
        Self {
            instance: value.instance,
            process_fixed: value.process_fixed,
            capabilities: value.capabilities,
            config_schema_path: value.config_schema_path,
            config_schema: value.config_schema,
        }
    }
}

impl From<crate::protocol::CompositionChangeSource> for CompositionChangeSource {
    fn from(value: crate::protocol::CompositionChangeSource) -> Self {
        match value {
            crate::protocol::CompositionChangeSource::Open => Self::Open,
            crate::protocol::CompositionChangeSource::Apply => Self::Apply,
            crate::protocol::CompositionChangeSource::PluginApply => Self::PluginApply,
        }
    }
}

impl From<crate::protocol::EventEnvelope> for HostEventRecord {
    fn from(value: crate::protocol::EventEnvelope) -> Self {
        let operation_id = (!value.command_id.starts_with("system:"))
            .then(|| OperationId(value.command_id.clone()));
        let event = match value.payload {
            crate::protocol::Event::CompositionCommitted {
                source,
                composition_id,
                manifest_sha256,
                lock_sha256,
                active_instances,
                inactive_instances,
            } => HostEvent::CompositionCommitted {
                source: source.into(),
                composition_id,
                manifest_sha256,
                lock_sha256,
                active_instances,
                inactive_instances,
            },
            crate::protocol::Event::HostShuttingDown => HostEvent::HostShuttingDown,
            crate::protocol::Event::RuntimeFaulted {
                instance_id,
                reason,
            } => HostEvent::RuntimeFaulted {
                instance_id,
                reason,
            },
            crate::protocol::Event::DaemonRestarting {
                source,
                composition_id,
                packages,
                candidate_manifest_sha256,
                candidate_lock_sha256,
            } => HostEvent::Unknown {
                event_type: "daemon_restarting".to_owned(),
                payload: serde_json::json!({
                    "source": source,
                    "composition_id": composition_id,
                    "packages": packages,
                    "candidate_manifest_sha256": candidate_manifest_sha256,
                    "candidate_lock_sha256": candidate_lock_sha256,
                })
                .as_object()
                .expect("object literal")
                .clone(),
            },
            crate::protocol::Event::Unknown {
                event_type,
                payload,
            } => HostEvent::Unknown {
                event_type,
                payload,
            },
        };
        Self {
            cursor: value.cursor,
            operation_id,
            graph_revision: value.graph_revision,
            event,
        }
    }
}

impl CompositionProject {
    /// Validates this candidate for the current process target without staging it.
    ///
    /// # Errors
    ///
    /// Returns an error for environmental or file-I/O failures. Candidate,
    /// schema, lock, and graph problems are returned as diagnostics.
    pub fn validate(&self) -> crate::Result<ValidationReport> {
        crate::composition::validate_project(self)
    }

    /// Creates the candidate lock, or verifies an equivalent existing lock.
    ///
    /// # Errors
    ///
    /// Returns an error when input cannot be read or validated, the lock cannot
    /// be atomically created, or an existing lock has different canonical content.
    pub fn lock(&self) -> crate::Result<LockResult> {
        crate::composition::lock_project(self)
    }
}
