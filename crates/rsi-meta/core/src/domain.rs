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
    pub database_path: PathBuf,
    pub cache_root: PathBuf,
    pub manifest_path: PathBuf,
    pub lock_path: PathBuf,
}

/// A composition candidate read independently from a live host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompositionProject {
    pub manifest_path: PathBuf,
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
    pub composition_id: String,
    pub manifest_sha256: String,
    pub lock_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplyRequest {
    pub operation_id: OperationId,
    pub project: CompositionProject,
    pub expected_revision: Option<GraphRevision>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallRequest {
    pub operation_id: OperationId,
    pub workspace: CompositionWorkspace,
    pub project: CompositionProject,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApplyResult {
    Applied {
        snapshot: HostSnapshot,
    },
    Unchanged {
        snapshot: HostSnapshot,
    },
    RestartRequired {
        current: Option<CompositionDigest>,
        candidate: CompositionDigest,
        packages: Vec<PackageId>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InstallResult {
    Installed { candidate: CompositionDigest },
    Unchanged { candidate: CompositionDigest },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LockResult {
    Created { lock: CompositionLock },
    Unchanged { lock: CompositionLock },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TokenRotation {
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShutdownReceipt {
    pub operation_id: OperationId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HostSnapshot {
    pub graph: GraphSnapshot,
    pub cursor: u64,
    pub token_generation: u64,
    pub active: Option<CompositionDigest>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionChangeSource {
    Open,
    Apply,
    PluginApply,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostEvent {
    CompositionCommitted {
        source: CompositionChangeSource,
        composition_id: String,
        manifest_sha256: String,
        lock_sha256: String,
        active_instances: u32,
        inactive_instances: u32,
    },
    RuntimeFaulted {
        instance_id: InstanceId,
        reason: String,
    },
    HostShuttingDown,
    Unknown {
        event_type: String,
        payload: serde_json::Map<String, serde_json::Value>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HostEventRecord {
    pub cursor: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<OperationId>,
    pub graph_revision: GraphRevision,
    pub event: HostEvent,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventPage {
    pub events: Vec<HostEventRecord>,
}

/// Descriptor-oriented inspect result for one mounted plugin instance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginInspection {
    pub instance: InstanceSnapshot,
    pub process_fixed: bool,
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema_path: Option<PathBuf>,
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
