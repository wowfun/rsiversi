//! Bounded invocation identity shared by draft, durable command and UI adapters.

use crate::{ContributionId, DomainRequestId, Result, SessionError, compact_json_len};
use serde::{Deserialize, Serialize};
mod receipt;
pub use receipt::{CommandOutcome, SessionCommandReceipt};

/// Maximum canonical argument bytes accepted by one Session command.
pub const MAXIMUM_COMMAND_ARGUMENT_BYTES: usize = 16 * 1024;
/// Maximum discoverable commands in one Session generation.
pub const MAXIMUM_SESSION_COMMANDS: usize = 64;

/// Exact predecessor in either the process-local draft or durable control stream.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CommandRevision {
    /// Draft-local revision; zero is the newly created payload.
    Draft {
        /// Monotonic mutation counter scoped to one draft lease.
        revision: u64,
    },
    /// Durable Session control cursor, independent of the Fact cursor.
    Durable {
        /// Exact committed control predecessor.
        control_seq: u64,
    },
}

/// Closed command arguments, bounded before callback dispatch or durable retention.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct CommandArguments(serde_json::Value);

impl CommandArguments {
    /// Validates JSON structure and canonical bytes; null is an explicit value.
    pub fn new(value: serde_json::Value) -> Result<Self> {
        rsi_ai_protocol::validate_json_structure(&value)
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        let actual = compact_json_len(&value)?;
        if actual > MAXIMUM_COMMAND_ARGUMENT_BYTES {
            return Err(SessionError::TooLarge {
                kind: "command arguments",
                maximum: MAXIMUM_COMMAND_ARGUMENT_BYTES,
                actual,
            });
        }
        Ok(Self(value))
    }

    /// Borrows bounded arguments for the registered command's semantic validation.
    pub const fn value(&self) -> &serde_json::Value {
        &self.0
    }
}

impl<'de> Deserialize<'de> for CommandArguments {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        Self::new(serde_json::Value::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Immutable logical invocation retained across retries, including its expected revision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCommandInvocation {
    /// Exact registered contribution identity; presentation names do not select execution.
    pub command: ContributionId,
    /// Caller-allocated idempotency identity, scoped to the Session or draft lease.
    pub request_id: DomainRequestId,
    /// Exact predecessor observed by the caller.
    pub expected_revision: CommandRevision,
    /// Arguments validated by the selected command before it returns proposals.
    pub arguments: CommandArguments,
}

impl SessionCommandInvocation {
    /// Returns the exact bounded canonical invocation digest, including JSON order and CAS.
    pub fn digest(&self) -> Result<String> {
        use sha2::{Digest as _, Sha256};
        let bytes =
            serde_json::to_vec(self).map_err(|error| SessionError::Encoding(error.to_string()))?;
        let mut hash = Sha256::new();
        hash.update(b"rsi-session-command-v1\0");
        hash.update(bytes);
        Ok(format!("{:x}", hash.finalize()))
    }
}

/// Bounded discovery metadata; selecting a command still uses its contribution identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCommandDescriptor {
    id: ContributionId,
    name: String,
    description: String,
    draft_safe: bool,
}

impl SessionCommandDescriptor {
    /// Creates discoverable command metadata; the name omits the presentation slash.
    pub fn new(
        id: ContributionId,
        name: impl Into<String>,
        description: impl Into<String>,
        draft_safe: bool,
    ) -> Result<Self> {
        let name = name.into();
        let description = description.into();
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(SessionError::Invalid(
                "command name must be bounded lowercase ASCII".into(),
            ));
        }
        crate::validate_safe_diagnostic("command description", &description)?;
        Ok(Self {
            id,
            name,
            description,
            draft_safe,
        })
    }
    /// Returns exact dispatch identity from the frozen generation.
    pub const fn id(&self) -> &ContributionId {
        &self.id
    }
    /// Returns the discoverable command name without a slash.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Returns safe bounded help text.
    pub fn description(&self) -> &str {
        &self.description
    }
    /// Reports whether this callback may propose initial state for an unpublished draft.
    pub const fn draft_safe(&self) -> bool {
        self.draft_safe
    }
}

impl<'de> Deserialize<'de> for SessionCommandDescriptor {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            id: ContributionId,
            name: String,
            description: String,
            draft_safe: bool,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.id, wire.name, wire.description, wire.draft_safe)
            .map_err(serde::de::Error::custom)
    }
}

/// One bounded discovery snapshot tied to the caller-visible predecessor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCommandsView {
    revision: CommandRevision,
    commands: Vec<SessionCommandDescriptor>,
}

impl SessionCommandsView {
    /// Validates finite, distinct command metadata at the discovery boundary.
    pub fn new(revision: CommandRevision, commands: Vec<SessionCommandDescriptor>) -> Result<Self> {
        if commands.len() > MAXIMUM_SESSION_COMMANDS {
            return Err(SessionError::TooLarge {
                kind: "Session commands",
                maximum: MAXIMUM_SESSION_COMMANDS,
                actual: commands.len(),
            });
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut names = std::collections::BTreeSet::new();
        for command in &commands {
            if !ids.insert(command.id()) || !names.insert(command.name()) {
                return Err(SessionError::Invalid(
                    "duplicate Session command identity or name".into(),
                ));
            }
        }
        Ok(Self { revision, commands })
    }
    /// Returns the captured draft or durable control predecessor.
    pub const fn revision(&self) -> CommandRevision {
        self.revision
    }
    /// Returns the frozen, ordered, bounded command metadata.
    pub fn commands(&self) -> &[SessionCommandDescriptor] {
        &self.commands
    }
}

impl<'de> Deserialize<'de> for SessionCommandsView {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            revision: CommandRevision,
            commands: Vec<SessionCommandDescriptor>,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.revision, wire.commands).map_err(serde::de::Error::custom)
    }
}
