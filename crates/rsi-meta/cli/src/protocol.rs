use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Result, bail};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use uuid::Uuid;

pub use rsi_meta::{
    CompositionDigest, GraphRevision, HostEvent as Event, HostEventRecord, InstanceId,
    MAX_WIRE_ID_BYTES, PluginInspection, STREAM_PROTOCOL, ServiceOpenRequest, StreamEnvelope,
    StreamId, StreamKind,
};
use rsi_meta::{GraphSnapshot, PackageId};

pub const CONTROL_PROTOCOL: &str = "rsi-meta.control";
pub const CONTROL_VERSION: u32 = 0;
pub const MAX_CONTROL_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

/// Closed top-level discriminator for daemon control envelopes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlEnvelopeKind {
    Command,
    Result,
    Event,
}

/// Versioned caller-to-daemon command envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommandEnvelope {
    /// Exact protocol identity; must equal [`CONTROL_PROTOCOL`].
    pub protocol: String,
    /// Exact protocol version; must equal [`CONTROL_VERSION`].
    pub version: u32,
    /// Must be [`ControlEnvelopeKind::Command`].
    pub kind: ControlEnvelopeKind,
    /// Caller-owned idempotency and result correlation identifier.
    pub command_id: String,
    /// Optional optimistic-concurrency precondition for graph mutation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_graph_revision: Option<GraphRevision>,
    pub payload: Command,
    /// Preserved forward-compatible envelope fields.
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

impl CommandEnvelope {
    /// Creates a command with the current protocol header.
    pub fn new(command_id: impl Into<String>, payload: Command) -> Self {
        Self {
            protocol: CONTROL_PROTOCOL.to_owned(),
            version: CONTROL_VERSION,
            kind: ControlEnvelopeKind::Command,
            command_id: command_id.into(),
            expected_graph_revision: None,
            payload,
            extensions: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_expected_revision(mut self, revision: GraphRevision) -> Self {
        self.expected_graph_revision = Some(revision);
        self
    }
}

/// Closed daemon control command vocabulary with unknown-command preservation.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Atomically applies one exact composition manifest/lock pair.
    ApplyManifestPath {
        /// Candidate manifest path resolved by the daemon before installation.
        manifest_path: PathBuf,
        /// Matching lock path.
        lock_path: PathBuf,
    },
    QueryGraph,
    /// Queries a bounded page of durable host events.
    QueryEvents {
        /// Return events strictly after this cursor.
        after_cursor: u64,
        /// Maximum events to return.
        limit: u32,
    },
    InspectPlugin {
        instance_id: InstanceId,
    },
    RotateToken,
    Shutdown,
    /// Forward-compatible command not understood by this daemon.
    Unknown {
        /// External command discriminator.
        command_type: String,
        /// Preserved command fields excluding the discriminator.
        payload: serde_json::Map<String, serde_json::Value>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum KnownCommand {
    ApplyManifestPath {
        manifest_path: PathBuf,
        lock_path: PathBuf,
    },
    QueryGraph,
    QueryEvents {
        #[serde(default)]
        after_cursor: u64,
        #[serde(default = "default_event_query_limit")]
        limit: u32,
    },
    InspectPlugin {
        instance_id: InstanceId,
    },
    RotateToken,
    Shutdown,
}

impl TryFrom<&Command> for KnownCommand {
    type Error = (String, serde_json::Map<String, serde_json::Value>);

    fn try_from(command: &Command) -> std::result::Result<Self, Self::Error> {
        Ok(match command {
            Command::ApplyManifestPath {
                manifest_path,
                lock_path,
            } => Self::ApplyManifestPath {
                manifest_path: manifest_path.clone(),
                lock_path: lock_path.clone(),
            },
            Command::QueryGraph => Self::QueryGraph,
            Command::QueryEvents {
                after_cursor,
                limit,
            } => Self::QueryEvents {
                after_cursor: *after_cursor,
                limit: *limit,
            },
            Command::InspectPlugin { instance_id } => Self::InspectPlugin {
                instance_id: instance_id.clone(),
            },
            Command::RotateToken => Self::RotateToken,
            Command::Shutdown => Self::Shutdown,
            Command::Unknown {
                command_type,
                payload,
            } => return Err((command_type.clone(), payload.clone())),
        })
    }
}

impl From<KnownCommand> for Command {
    fn from(command: KnownCommand) -> Self {
        match command {
            KnownCommand::ApplyManifestPath {
                manifest_path,
                lock_path,
            } => Self::ApplyManifestPath {
                manifest_path,
                lock_path,
            },
            KnownCommand::QueryGraph => Self::QueryGraph,
            KnownCommand::QueryEvents {
                after_cursor,
                limit,
            } => Self::QueryEvents {
                after_cursor,
                limit,
            },
            KnownCommand::InspectPlugin { instance_id } => Self::InspectPlugin { instance_id },
            KnownCommand::RotateToken => Self::RotateToken,
            KnownCommand::Shutdown => Self::Shutdown,
        }
    }
}

impl Serialize for Command {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match KnownCommand::try_from(self) {
            Ok(known) => known.serialize(serializer),
            Err((command_type, payload)) => {
                let mut map = serializer.serialize_map(Some(payload.len() + 1))?;
                map.serialize_entry("type", &command_type)?;
                for (key, value) in payload {
                    map.serialize_entry(&key, &value)?;
                }
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Command {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut object = serde_json::Map::<String, serde_json::Value>::deserialize(deserializer)?;
        let command_type = object
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| de::Error::missing_field("type"))?
            .to_owned();
        if matches!(
            command_type.as_str(),
            "apply_manifest_path"
                | "query_graph"
                | "query_events"
                | "inspect_plugin"
                | "rotate_token"
                | "shutdown"
        ) {
            return serde_json::from_value::<KnownCommand>(serde_json::Value::Object(object))
                .map(Into::into)
                .map_err(de::Error::custom);
        }
        object.remove("type");
        Ok(Self::Unknown {
            command_type,
            payload: object,
        })
    }
}

const fn default_event_query_limit() -> u32 {
    1_000
}

/// Versioned correlated result of one daemon command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommandOutcomeEnvelope {
    /// Exact protocol identity; must equal [`CONTROL_PROTOCOL`].
    pub protocol: String,
    /// Exact protocol version; must equal [`CONTROL_VERSION`].
    pub version: u32,
    /// Must be [`ControlEnvelopeKind::Result`].
    pub kind: ControlEnvelopeKind,
    /// Identifier from the corresponding command.
    pub command_id: String,
    /// Committed graph revision after processing the command.
    pub graph_revision: GraphRevision,
    pub payload: CommandOutcome,
    /// Preserved forward-compatible envelope fields.
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

impl CommandOutcomeEnvelope {
    /// Creates a bounded result, replacing oversized payloads with rejection.
    pub fn new(
        command_id: impl Into<String>,
        graph_revision: GraphRevision,
        payload: CommandOutcome,
    ) -> Self {
        let outcome = Self {
            protocol: CONTROL_PROTOCOL.to_owned(),
            version: CONTROL_VERSION,
            kind: ControlEnvelopeKind::Result,
            command_id: command_id.into(),
            graph_revision,
            payload,
            extensions: BTreeMap::new(),
        };
        if serde_json::to_vec(&outcome)
            .is_ok_and(|encoded| encoded.len() <= MAX_CONTROL_RESPONSE_BYTES)
        {
            outcome
        } else {
            Self {
                payload: CommandOutcome::Rejected {
                    code: "outcome_too_large".to_owned(),
                    message: "command result exceeds the control response limit".to_owned(),
                    details: BTreeMap::new(),
                },
                ..outcome
            }
        }
    }
}

/// Closed semantic result vocabulary for daemon commands.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum CommandOutcome {
    Applied {
        graph: GraphSnapshot,
    },
    NoChange {
        graph: GraphSnapshot,
    },
    /// Process-fixed package changes require daemon restart.
    RestartRequired {
        /// Currently active composition, if any.
        current: Option<CompositionDigest>,
        /// Validated desired composition.
        candidate: CompositionDigest,
        /// Packages requiring a new process.
        packages: Vec<PackageId>,
    },
    Graph {
        graph: GraphSnapshot,
        /// Last durable host-event cursor.
        cursor: u64,
    },
    /// Bounded durable event query result.
    Events {
        /// Events ordered by increasing cursor.
        events: Vec<EventEnvelope>,
    },
    Plugin {
        instance: Option<PluginInspection>,
    },
    TokenRotated {
        /// New monotonic token generation.
        generation: u64,
    },
    /// Command was safely rejected without the requested mutation.
    Rejected {
        /// Stable machine-readable rejection code.
        code: String,
        /// Bounded human-readable rejection summary.
        message: String,
        /// Structured bounded rejection metadata.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        details: BTreeMap<String, serde_json::Value>,
    },
    ShuttingDown,
}

/// Versioned durable host event delivered by query or subscription.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Exact protocol identity; must equal [`CONTROL_PROTOCOL`].
    pub protocol: String,
    /// Exact protocol version; must equal [`CONTROL_VERSION`].
    pub version: u32,
    /// Must be [`ControlEnvelopeKind::Event`].
    pub kind: ControlEnvelopeKind,
    /// Monotonic durable event cursor.
    pub cursor: u64,
    /// Operation that caused the event, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    /// Graph revision in effect when the event was committed.
    pub graph_revision: GraphRevision,
    pub payload: Event,
    /// Preserved forward-compatible envelope fields.
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

impl From<HostEventRecord> for EventEnvelope {
    fn from(record: HostEventRecord) -> Self {
        Self {
            protocol: CONTROL_PROTOCOL.to_owned(),
            version: CONTROL_VERSION,
            kind: ControlEnvelopeKind::Event,
            cursor: record.cursor,
            operation_id: record.operation_id.map(|operation| operation.0),
            graph_revision: record.graph_revision,
            payload: record.event,
            extensions: BTreeMap::new(),
        }
    }
}

/// Parsed one-shot CLI request before conversion to the daemon wire contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CliRequest {
    ApplyManifest {
        manifest: PathBuf,
        lock: PathBuf,
        /// Caller-owned idempotency identity.
        operation_id: String,
    },
    QueryGraph,
    /// Queries a bounded durable event page.
    QueryEvents {
        /// Return events strictly after this cursor.
        after: u64,
        /// Maximum events to return.
        limit: u32,
    },
    InspectPlugin {
        instance_id: String,
    },
    RotateToken {
        /// Caller-owned idempotency identity.
        operation_id: String,
    },
    Shutdown {
        /// Caller-owned idempotency identity.
        operation_id: String,
    },
}

impl CliRequest {
    pub fn into_envelope(self) -> CommandEnvelope {
        let (command_id, command) = match self {
            Self::ApplyManifest {
                manifest,
                lock,
                operation_id,
            } => (
                operation_id,
                Command::ApplyManifestPath {
                    manifest_path: manifest,
                    lock_path: lock,
                },
            ),
            Self::QueryGraph => (Uuid::now_v7().to_string(), Command::QueryGraph),
            Self::QueryEvents { after, limit } => (
                Uuid::now_v7().to_string(),
                Command::QueryEvents {
                    after_cursor: after,
                    limit,
                },
            ),
            Self::InspectPlugin { instance_id } => (
                Uuid::now_v7().to_string(),
                Command::InspectPlugin {
                    instance_id: InstanceId::new(instance_id),
                },
            ),
            Self::RotateToken { operation_id } => (operation_id, Command::RotateToken),
            Self::Shutdown { operation_id } => (operation_id, Command::Shutdown),
        };
        CommandEnvelope::new(command_id, command)
    }
}

/// Validates protocol metadata and command-specific envelope fields.
///
/// # Errors
///
/// Returns an error for unsupported protocol metadata, an invalid command ID,
/// a non-command envelope, or a revision precondition on a non-apply command.
pub fn validate_command(envelope: &CommandEnvelope) -> Result<()> {
    if envelope.protocol != CONTROL_PROTOCOL || envelope.version != CONTROL_VERSION {
        bail!(
            "unsupported protocol {:?} version {}",
            envelope.protocol,
            envelope.version
        );
    }
    if envelope.kind != ControlEnvelopeKind::Command {
        bail!("expected a command envelope");
    }
    if envelope.command_id.is_empty()
        || envelope.command_id.len() > MAX_WIRE_ID_BYTES
        || !envelope
            .command_id
            .bytes()
            .all(|byte| byte.is_ascii_graphic())
    {
        bail!("command_id must contain 1 to 255 printable ASCII bytes without spaces");
    }
    if let Some(field) = ["operation_id", "cursor", "graph_revision"]
        .into_iter()
        .find(|field| envelope.extensions.contains_key(*field))
    {
        bail!("field {field:?} is not valid on a command envelope");
    }
    if envelope.expected_graph_revision.is_some()
        && !matches!(envelope.payload, Command::ApplyManifestPath { .. })
    {
        bail!("expected_graph_revision is only valid for apply_manifest_path");
    }
    Ok(())
}

/// Validates protocol metadata and result correlation fields.
///
/// # Errors
///
/// Returns an error for unsupported protocol metadata, a non-result envelope,
/// or a missing result correlation ID.
pub fn validate_outcome(envelope: &CommandOutcomeEnvelope) -> Result<()> {
    if envelope.protocol != CONTROL_PROTOCOL || envelope.version != CONTROL_VERSION {
        bail!("unsupported control result protocol");
    }
    if envelope.kind != ControlEnvelopeKind::Result || envelope.command_id.is_empty() {
        bail!("invalid result envelope");
    }
    Ok(())
}

pub fn rejected(
    command_id: impl Into<String>,
    graph_revision: GraphRevision,
    code: impl Into<String>,
    message: impl Into<String>,
) -> CommandOutcomeEnvelope {
    CommandOutcomeEnvelope::new(
        command_id,
        graph_revision,
        CommandOutcome::Rejected {
            code: code.into(),
            message: message.into(),
            details: BTreeMap::new(),
        },
    )
}

#[cfg(test)]
pub(crate) fn outcome(
    command_id: impl Into<String>,
    graph_revision: GraphRevision,
    payload: CommandOutcome,
) -> CommandOutcomeEnvelope {
    CommandOutcomeEnvelope::new(command_id, graph_revision, payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_ids_are_correlation_only_and_revision_is_apply_only() {
        let query = CliRequest::QueryGraph.into_envelope();
        validate_command(&query).unwrap();
        let stale_query = query.with_expected_revision(GraphRevision(1));
        assert!(validate_command(&stale_query).is_err());
    }

    #[test]
    fn unknown_commands_round_trip() {
        let encoded = serde_json::json!({
            "protocol": CONTROL_PROTOCOL,
            "version": CONTROL_VERSION,
            "kind": "command",
            "command_id": "future",
            "payload": { "type": "future_command", "value": 1 }
        });
        let decoded: CommandEnvelope = serde_json::from_value(encoded).unwrap();
        assert!(matches!(decoded.payload, Command::Unknown { .. }));
    }

    #[test]
    fn command_validation_rejects_kind_forbidden_envelope_fields() {
        for forbidden in ["operation_id", "cursor", "graph_revision"] {
            let mut encoded = serde_json::json!({
                "protocol": CONTROL_PROTOCOL,
                "version": CONTROL_VERSION,
                "kind": "command",
                "command_id": "forbidden-field",
                "payload": { "type": "query_graph" }
            });
            encoded[forbidden] = serde_json::json!(1);
            let decoded: CommandEnvelope = serde_json::from_value(encoded).unwrap();
            assert!(
                validate_command(&decoded).is_err(),
                "command field {forbidden:?} is forbidden by the published schema"
            );
        }
    }
}
