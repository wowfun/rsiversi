use std::collections::BTreeMap;

use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::domain::CompositionDigest;
use crate::model::{
    CompositionLock, GraphRevision, GraphSnapshot, InstanceId, InstanceSnapshot, ValidationReport,
};

pub const CONTROL_PROTOCOL: &str = "rsi-meta.control";
pub const CONTROL_VERSION: u32 = 0;
pub const STREAM_PROTOCOL: &str = "rsi-meta.stream";
pub const STREAM_VERSION: u32 = 0;
/// Maximum Unicode-scalar length of connection- and persistence-retained wire identifiers.
pub const MAX_WIRE_ID_CHARACTERS: usize = 255;
/// Maximum serialized size of a durable control result and its wire frame.
pub const MAX_CONTROL_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlEnvelopeKind {
    Command,
    Result,
    Event,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommandEnvelope {
    pub protocol: String,
    pub version: u32,
    pub kind: ControlEnvelopeKind,
    pub command_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_graph_revision: Option<GraphRevision>,
    pub payload: Command,
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

impl CommandEnvelope {
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

    /// Validates the control protocol header and bounded durable identifier.
    ///
    /// # Errors
    ///
    /// Returns an envelope or protocol error when the header, kind, or command
    /// identifier is outside the version-zero contract.
    pub fn validate(&self) -> crate::Result<()> {
        if self.protocol != CONTROL_PROTOCOL || self.version != CONTROL_VERSION {
            return Err(crate::HostError::UnsupportedProtocol {
                protocol: self.protocol.clone(),
                version: self.version,
            });
        }
        if self.kind != ControlEnvelopeKind::Command {
            return Err(crate::HostError::InvalidEnvelope(
                "command envelope kind must be command".to_owned(),
            ));
        }
        if self.command_id.is_empty()
            || self.command_id.len() > MAX_WIRE_ID_CHARACTERS
            || !self.command_id.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(crate::HostError::InvalidEnvelope(format!(
                "command_id must contain 1 to {MAX_WIRE_ID_CHARACTERS} printable ASCII bytes without spaces"
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    ApplyManifestPath {
        manifest_path: std::path::PathBuf,
        lock_path: std::path::PathBuf,
    },
    ValidateManifest {
        manifest_path: std::path::PathBuf,
        lock_path: Option<std::path::PathBuf>,
    },
    LockManifest {
        manifest_path: std::path::PathBuf,
        lock_path: std::path::PathBuf,
    },
    QueryGraph,
    QueryEvents {
        after_cursor: u64,
        limit: u32,
    },
    InspectPlugin {
        instance_id: InstanceId,
    },
    RotateToken,
    Shutdown,
    /// A forward-version command retained until the registry can return a
    /// structured `unsupported_command` outcome. Unknown commands must not
    /// tear down a transport while their envelope is otherwise valid.
    Unknown {
        command_type: String,
        payload: serde_json::Map<String, serde_json::Value>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum KnownCommand {
    ApplyManifestPath {
        manifest_path: std::path::PathBuf,
        lock_path: std::path::PathBuf,
    },
    ValidateManifest {
        manifest_path: std::path::PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lock_path: Option<std::path::PathBuf>,
    },
    LockManifest {
        manifest_path: std::path::PathBuf,
        lock_path: std::path::PathBuf,
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
            KnownCommand::ValidateManifest {
                manifest_path,
                lock_path,
            } => Self::ValidateManifest {
                manifest_path,
                lock_path,
            },
            KnownCommand::LockManifest {
                manifest_path,
                lock_path,
            } => Self::LockManifest {
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
            Command::ValidateManifest {
                manifest_path,
                lock_path,
            } => Self::ValidateManifest {
                manifest_path: manifest_path.clone(),
                lock_path: lock_path.clone(),
            },
            Command::LockManifest {
                manifest_path,
                lock_path,
            } => Self::LockManifest {
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
                | "validate_manifest"
                | "lock_manifest"
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

fn default_event_query_limit() -> u32 {
    1_000
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommandOutcomeEnvelope {
    pub protocol: String,
    pub version: u32,
    pub kind: ControlEnvelopeKind,
    pub command_id: String,
    pub graph_revision: GraphRevision,
    pub payload: CommandOutcome,
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

impl CommandOutcomeEnvelope {
    pub(crate) fn new(
        command_id: String,
        graph_revision: GraphRevision,
        payload: CommandOutcome,
    ) -> Self {
        let outcome = Self {
            protocol: CONTROL_PROTOCOL.to_owned(),
            version: CONTROL_VERSION,
            kind: ControlEnvelopeKind::Result,
            command_id,
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
                protocol: CONTROL_PROTOCOL.to_owned(),
                version: CONTROL_VERSION,
                kind: ControlEnvelopeKind::Result,
                command_id: outcome.command_id,
                graph_revision: outcome.graph_revision,
                payload: CommandOutcome::Rejected {
                    code: "outcome_too_large".to_owned(),
                    message: "command result exceeds the durable control response limit".to_owned(),
                },
                extensions: BTreeMap::new(),
            }
        }
    }

    pub fn rejected(
        command_id: impl Into<String>,
        graph_revision: GraphRevision,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(
            command_id.into(),
            graph_revision,
            CommandOutcome::Rejected {
                code: code.into(),
                message: message.into(),
            },
        )
    }

    pub fn token_rotated(
        command_id: impl Into<String>,
        graph_revision: GraphRevision,
        generation: u64,
    ) -> Self {
        Self::new(
            command_id.into(),
            graph_revision,
            CommandOutcome::TokenRotated { generation },
        )
    }
}

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
    Validation {
        report: ValidationReport,
    },
    LockResolved {
        lock: CompositionLock,
    },
    Graph {
        graph: GraphSnapshot,
        /// Durable event cursor captured with the committed graph and revision.
        /// Retiring entries in `graph` are a best-effort lifecycle overlay.
        /// Clients subscribe strictly after this value.
        #[serde(default)]
        cursor: u64,
    },
    Events {
        events: Vec<EventEnvelope>,
    },
    Plugin {
        instance: Option<PluginInspection>,
    },
    TokenRotated {
        generation: u64,
    },
    RestartRequired {
        current: Option<CompositionDigest>,
        candidate: CompositionDigest,
        packages: Vec<crate::model::PackageId>,
    },
    Installed {
        candidate: CompositionDigest,
        changed: bool,
    },
    Rejected {
        code: String,
        message: String,
    },
    ShuttingDown,
}

/// Descriptor-oriented inspect DTO. Routing snapshots stay compact while
/// callers can inspect the package-owned contract and exact staged artifact.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginInspection {
    pub instance: InstanceSnapshot,
    pub process_fixed: bool,
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema_path: Option<std::path::PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub protocol: String,
    pub version: u32,
    pub kind: ControlEnvelopeKind,
    pub cursor: u64,
    pub command_id: String,
    pub graph_revision: GraphRevision,
    pub payload: Event,
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

impl EventEnvelope {
    pub(crate) fn new(
        command_id: impl Into<String>,
        cursor: u64,
        graph_revision: GraphRevision,
        payload: Event,
    ) -> Self {
        Self {
            protocol: CONTROL_PROTOCOL.to_owned(),
            version: CONTROL_VERSION,
            kind: ControlEnvelopeKind::Event,
            cursor,
            command_id: command_id.into(),
            graph_revision,
            payload,
            extensions: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    CompositionCommitted {
        source: CompositionChangeSource,
        composition_id: String,
        manifest_sha256: String,
        lock_sha256: String,
        active_instances: u32,
        inactive_instances: u32,
    },
    DaemonRestarting {
        source: CompositionChangeSource,
        composition_id: String,
        packages: Vec<crate::model::PackageId>,
        candidate_manifest_sha256: String,
        candidate_lock_sha256: String,
    },
    HostShuttingDown,
    /// A future event retained byte-for-byte at the JSON value level so an
    /// older daemon can replay a newer audit log without failing startup.
    Unknown {
        event_type: String,
        payload: serde_json::Map<String, serde_json::Value>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum KnownEvent {
    CompositionCommitted {
        source: CompositionChangeSource,
        composition_id: String,
        manifest_sha256: String,
        lock_sha256: String,
        active_instances: u32,
        inactive_instances: u32,
    },
    DaemonRestarting {
        source: CompositionChangeSource,
        composition_id: String,
        packages: Vec<crate::model::PackageId>,
        candidate_manifest_sha256: String,
        candidate_lock_sha256: String,
    },
    HostShuttingDown,
}

impl From<KnownEvent> for Event {
    fn from(event: KnownEvent) -> Self {
        match event {
            KnownEvent::CompositionCommitted {
                source,
                composition_id,
                manifest_sha256,
                lock_sha256,
                active_instances,
                inactive_instances,
            } => Self::CompositionCommitted {
                source,
                composition_id,
                manifest_sha256,
                lock_sha256,
                active_instances,
                inactive_instances,
            },
            KnownEvent::DaemonRestarting {
                source,
                composition_id,
                packages,
                candidate_manifest_sha256,
                candidate_lock_sha256,
            } => Self::DaemonRestarting {
                source,
                composition_id,
                packages,
                candidate_manifest_sha256,
                candidate_lock_sha256,
            },
            KnownEvent::HostShuttingDown => Self::HostShuttingDown,
        }
    }
}

impl TryFrom<&Event> for KnownEvent {
    type Error = (String, serde_json::Map<String, serde_json::Value>);

    fn try_from(event: &Event) -> std::result::Result<Self, Self::Error> {
        Ok(match event {
            Event::CompositionCommitted {
                source,
                composition_id,
                manifest_sha256,
                lock_sha256,
                active_instances,
                inactive_instances,
            } => Self::CompositionCommitted {
                source: *source,
                composition_id: composition_id.clone(),
                manifest_sha256: manifest_sha256.clone(),
                lock_sha256: lock_sha256.clone(),
                active_instances: *active_instances,
                inactive_instances: *inactive_instances,
            },
            Event::DaemonRestarting {
                source,
                composition_id,
                packages,
                candidate_manifest_sha256,
                candidate_lock_sha256,
            } => Self::DaemonRestarting {
                source: *source,
                composition_id: composition_id.clone(),
                packages: packages.clone(),
                candidate_manifest_sha256: candidate_manifest_sha256.clone(),
                candidate_lock_sha256: candidate_lock_sha256.clone(),
            },
            Event::HostShuttingDown => Self::HostShuttingDown,
            Event::Unknown {
                event_type,
                payload,
            } => return Err((event_type.clone(), payload.clone())),
        })
    }
}

impl Serialize for Event {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match KnownEvent::try_from(self) {
            Ok(known) => known.serialize(serializer),
            Err((event_type, payload)) => {
                let mut map = serializer.serialize_map(Some(payload.len() + 1))?;
                map.serialize_entry("type", &event_type)?;
                for (key, value) in payload {
                    map.serialize_entry(&key, &value)?;
                }
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Event {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut object = serde_json::Map::<String, serde_json::Value>::deserialize(deserializer)?;
        let event_type = object
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| de::Error::missing_field("type"))?
            .to_owned();
        if matches!(
            event_type.as_str(),
            "composition_committed" | "daemon_restarting" | "host_shutting_down"
        ) {
            return serde_json::from_value::<KnownEvent>(serde_json::Value::Object(object))
                .map(Into::into)
                .map_err(de::Error::custom);
        }
        object.remove("type");
        Ok(Self::Unknown {
            event_type,
            payload: object,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionChangeSource {
    Open,
    Apply,
    PluginApply,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    Open,
    Data,
    Credit,
    HalfClose,
    Cancel,
    End,
}

/// Transport-neutral stream frame. Flow control is expressed in bytes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamEnvelope {
    pub protocol: String,
    pub version: u32,
    pub kind: StreamKind,
    pub stream_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

impl StreamEnvelope {
    pub fn new(stream_id: impl Into<String>, kind: StreamKind) -> Self {
        Self {
            protocol: STREAM_PROTOCOL.to_owned(),
            version: STREAM_VERSION,
            kind,
            stream_id: stream_id.into(),
            sequence: None,
            credit_bytes: None,
            payload: None,
            extensions: BTreeMap::new(),
        }
    }

    /// Validates protocol identity and kind-specific credit fields.
    ///
    /// # Errors
    ///
    /// Returns an envelope/protocol error for malformed frames.
    pub fn validate(&self) -> crate::Result<()> {
        if self.protocol != STREAM_PROTOCOL || self.version != STREAM_VERSION {
            return Err(crate::HostError::UnsupportedProtocol {
                protocol: self.protocol.clone(),
                version: self.version,
            });
        }
        if self.stream_id.is_empty() || self.stream_id.chars().count() > MAX_WIRE_ID_CHARACTERS {
            return Err(crate::HostError::InvalidEnvelope(format!(
                "stream_id must contain 1 to {MAX_WIRE_ID_CHARACTERS} characters"
            )));
        }
        match self.kind {
            StreamKind::Credit if self.credit_bytes.is_none() => Err(
                crate::HostError::InvalidEnvelope("credit frames require credit_bytes".to_owned()),
            ),
            StreamKind::Credit if self.sequence.is_some() || self.payload.is_some() => {
                Err(crate::HostError::InvalidEnvelope(
                    "credit frames cannot carry sequence or payload".to_owned(),
                ))
            }
            StreamKind::Credit => Ok(()),
            _ if self.credit_bytes.is_some() => Err(crate::HostError::InvalidEnvelope(
                "credit_bytes is only valid on credit frames".to_owned(),
            )),
            StreamKind::Open if !valid_open_payload(self.payload.as_ref()) => {
                Err(crate::HostError::InvalidEnvelope(
                    "open payload must contain exactly consumer+service or provider".to_owned(),
                ))
            }
            StreamKind::Data if self.sequence.is_none() || self.payload.is_none() => {
                Err(crate::HostError::InvalidEnvelope(
                    "data frames require sequence and payload".to_owned(),
                ))
            }
            StreamKind::Data if self.sequence == Some(0) => Err(crate::HostError::InvalidEnvelope(
                "data frame sequence must be greater than zero".to_owned(),
            )),
            StreamKind::Data
                if self.payload.as_ref().is_none_or(|payload| {
                    payload.as_array().is_none_or(|bytes| {
                        bytes
                            .iter()
                            .any(|byte| byte.as_u64().is_none_or(|byte| byte > u64::from(u8::MAX)))
                    })
                }) =>
            {
                Err(crate::HostError::InvalidEnvelope(
                    "data frame payload must be a JSON byte array".to_owned(),
                ))
            }
            StreamKind::Open | StreamKind::HalfClose | StreamKind::Cancel | StreamKind::End
                if self.sequence.is_some() =>
            {
                Err(crate::HostError::InvalidEnvelope(
                    "sequence is only valid on data frames".to_owned(),
                ))
            }
            StreamKind::HalfClose if self.payload.is_some() => {
                Err(crate::HostError::InvalidEnvelope(
                    "half_close frames cannot carry payload".to_owned(),
                ))
            }
            StreamKind::Cancel
                if self.payload.as_ref().is_none_or(|payload| {
                    payload
                        .as_object()
                        .and_then(|payload| payload.get("reason"))
                        .and_then(serde_json::Value::as_str)
                        .is_none_or(str::is_empty)
                }) =>
            {
                Err(crate::HostError::InvalidEnvelope(
                    "cancel payload requires a non-empty reason".to_owned(),
                ))
            }
            _ => Ok(()),
        }
    }
}

fn valid_open_payload(payload: Option<&serde_json::Value>) -> bool {
    let Some(payload) = payload.and_then(serde_json::Value::as_object) else {
        return false;
    };
    let consumer_route = payload.len() == 2
        && payload
            .get("consumer")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.is_empty())
        && payload
            .get("service")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.is_empty());
    let provider_route = payload.len() == 1
        && payload
            .get("provider")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.is_empty());
    consumer_route || provider_route
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_control_and_event_payloads_round_trip() {
        let command_json = r#"{"type":"future_command","answer":42}"#;
        let command: Command = serde_json::from_str(command_json).expect("unknown command");
        assert_eq!(serde_json::to_string(&command).unwrap(), command_json);

        let event_json = r#"{"type":"future_event","nested":{"ok":true}}"#;
        let event: Event = serde_json::from_str(event_json).expect("unknown event");
        assert_eq!(serde_json::to_string(&event).unwrap(), event_json);
    }

    #[test]
    fn legacy_stored_graph_outcomes_resume_with_a_conservative_cursor() {
        let outcome: CommandOutcome = serde_json::from_value(serde_json::json!({
            "type": "graph",
            "graph": {
                "revision": 3,
                "composition_id": "legacy",
                "instances": {},
                "bindings": []
            }
        }))
        .expect("pre-cursor v0 graph outcome remains resumable");
        assert!(matches!(outcome, CommandOutcome::Graph { cursor: 0, .. }));
    }

    #[test]
    fn stream_identifiers_are_bounded_before_connection_state_retains_them() {
        let frame = StreamEnvelope::new("s".repeat(256), StreamKind::Credit);
        let mut frame = frame;
        frame.credit_bytes = Some(1);

        assert!(frame.validate().is_err());

        let mut unicode = StreamEnvelope::new("界".repeat(255), StreamKind::Credit);
        unicode.credit_bytes = Some(1);
        assert!(unicode.validate().is_ok());
        unicode.stream_id.push('界');
        assert!(unicode.validate().is_err());
    }

    #[test]
    fn oversized_command_outcomes_become_small_persistable_rejections() {
        let outcome = CommandOutcomeEnvelope::new(
            "oversized".to_owned(),
            GraphRevision(0),
            CommandOutcome::Rejected {
                code: "fixture".to_owned(),
                message: "x".repeat(MAX_CONTROL_RESPONSE_BYTES),
            },
        );

        assert!(matches!(
            outcome.payload,
            CommandOutcome::Rejected { ref code, .. } if code == "outcome_too_large"
        ));
        assert!(serde_json::to_vec(&outcome).unwrap().len() <= MAX_CONTROL_RESPONSE_BYTES);
    }

    #[test]
    fn command_identifier_matches_the_printable_ascii_json_schema_contract() {
        let command = CommandEnvelope::new("x".repeat(255), Command::QueryGraph);
        assert!(command.validate().is_ok());
        let command = CommandEnvelope::new("x".repeat(256), Command::QueryGraph);
        assert!(command.validate().is_err());
        assert!(
            CommandEnvelope::new("unsafe\nlog", Command::QueryGraph)
                .validate()
                .is_err()
        );
        assert!(
            CommandEnvelope::new("界", Command::QueryGraph)
                .validate()
                .is_err()
        );
    }

    #[test]
    fn stream_validation_matches_kind_specific_schema() {
        let mut open = StreamEnvelope::new("stream", StreamKind::Open);
        assert!(open.validate().is_err());
        open.payload = Some(serde_json::json!({"consumer":"caller","service":"echo"}));
        assert!(open.validate().is_ok());
        open.payload = Some(serde_json::json!({"provider":"echo"}));
        assert!(open.validate().is_ok());
        open.payload = Some(serde_json::json!({"provider":"echo","extra":true}));
        assert!(open.validate().is_err());

        let mut data = StreamEnvelope::new("stream", StreamKind::Data);
        data.payload = Some(serde_json::json!([1]));
        assert!(data.validate().is_err());
        data.sequence = Some(1);
        assert!(data.validate().is_ok());
        data.sequence = Some(0);
        assert!(data.validate().is_err());
        data.sequence = Some(1);
        data.payload = Some(serde_json::json!([256]));
        assert!(data.validate().is_err());
        data.payload = Some(serde_json::json!(["not-a-byte"]));
        assert!(data.validate().is_err());

        let mut cancel = StreamEnvelope::new("stream", StreamKind::Cancel);
        cancel.sequence = Some(1);
        assert!(cancel.validate().is_err());
        cancel.sequence = None;
        assert!(cancel.validate().is_err());
        cancel.payload = Some(serde_json::json!({"reason":""}));
        assert!(cancel.validate().is_err());
        cancel.payload = Some(serde_json::json!({"reason":"client_closed"}));
        assert!(cancel.validate().is_ok());

        let mut half_close = StreamEnvelope::new("stream", StreamKind::HalfClose);
        half_close.payload = Some(serde_json::json!({}));
        assert!(half_close.validate().is_err());
    }
}
