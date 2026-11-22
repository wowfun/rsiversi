//! Bounded invocation identity shared by draft, durable command and UI adapters.

use crate::{
    ContributionId, DomainRequestId, MessageAttribution, Result, SessionError, compact_json_len,
};
use serde::{Deserialize, Serialize};
mod receipt;
pub use receipt::{CommandOutcome, SessionCommandReceipt};

/// Maximum canonical argument bytes accepted by one Session command.
pub const MAXIMUM_COMMAND_ARGUMENT_BYTES: usize = 64 * 1024;
/// Maximum discoverable commands in one Session generation.
pub const MAXIMUM_SESSION_COMMANDS: usize = 64;
/// Maximum messages produced by one command or retained in an unpublished draft.
pub const MAXIMUM_COMMAND_INPUTS: usize = 64;
/// Maximum complete encoded command input batch, including provenance.
pub const MAXIMUM_COMMAND_INPUT_BYTES: usize = 256 * 1024;

/// Exact mailbox input bound to a domain publication, before active-Turn routing.
#[derive(schemars::JsonSchema, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandMessage {
    /// Immutable command source, identity and typed content.
    pub message: crate::AgentMessage,
    /// Mailbox delivery intent, including the task that starts a fresh Session.
    pub delivery: crate::MessageDelivery,
}
impl CommandMessage {
    /// Exact compact publication size, including scheduling intent and provenance.
    pub fn encoded_len(&self) -> Result<usize> {
        compact_json_len(self)
    }
    /// Validates ordering, provenance and aggregate bounds for one command-input batch.
    /// An initial publication can end with the task that starts its first turn.
    pub fn validate_batch(
        inputs: &[Self],
        invocation: Option<&SessionCommandInvocation>,
        draft: bool,
    ) -> Result<()> {
        if inputs.len() > MAXIMUM_COMMAND_INPUTS {
            return Err(SessionError::Invalid("command input capacity".into()));
        }
        if inputs.is_empty() {
            return Ok(());
        }
        let binding = invocation
            .map(|invocation| invocation.digest().map(|digest| (invocation, digest)))
            .transpose()?;
        let mut ids = std::collections::BTreeSet::new();
        let mut ordinals = std::collections::BTreeMap::new();
        for (index, input) in inputs.iter().enumerate() {
            let source = input.validated_source()?;
            if let Some((invocation, digest)) = &binding
                && (source.command != invocation.command
                    || source.request_id != invocation.request_id
                    || source.invocation_sha256 != *digest)
            {
                return Err(SessionError::Invalid(
                    "command input changed invocation".into(),
                ));
            }
            let expected = ordinals
                .entry((&source.request_id, &source.invocation_sha256))
                .or_insert(0u32);
            if source.ordinal != *expected {
                return Err(SessionError::Invalid(
                    "command inputs changed their output order".into(),
                ));
            }
            *expected += 1;
            if !ids.insert(&input.message.message_id) {
                return Err(SessionError::Invalid("duplicate command input".into()));
            }
            if draft
                && input.delivery != crate::MessageDelivery::NextStep
                && !(index + 1 == inputs.len()
                    && input.delivery == crate::MessageDelivery::NextTurn)
            {
                return Err(SessionError::Invalid(
                    "only the last initial command input can start the first turn".into(),
                ));
            }
        }
        if compact_json_len(&inputs)? > MAXIMUM_COMMAND_INPUT_BYTES {
            return Err(SessionError::Invalid("command input byte capacity".into()));
        }
        Ok(())
    }
    fn validated_source(&self) -> Result<&CommandMessageSource> {
        self.message.validate()?;
        let crate::AgentMessageSource::Command { source } = &self.message.source else {
            return Err(SessionError::Invalid(
                "command message changed source".into(),
            ));
        };
        Ok(source)
    }
}

/// Immutable provenance assigned by a command owner, never a dispatch credential.
#[derive(schemars::JsonSchema, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandMessageSource {
    /// Registered producer identity.
    pub command: ContributionId,
    /// Exact command request.
    pub request_id: DomainRequestId,
    /// Digest of the complete immutable invocation.
    pub invocation_sha256: String,
    /// Zero-based position in that command's returned input batch.
    pub ordinal: u32,
    /// Original presentation metadata; native identity remains invocation-derived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<MessageAttribution>,
}
impl CommandMessageSource {
    /// Checks digest and ordinal at the durable boundary.
    pub fn validate(&self) -> Result<()> {
        if let Some(attribution) = &self.attribution {
            attribution.validate()?;
        }
        crate::validate_sha256("command invocation", &self.invocation_sha256)?;
        if self.ordinal as usize >= MAXIMUM_COMMAND_INPUTS {
            return Err(SessionError::Invalid(
                "command input ordinal exceeds its bound".into(),
            ));
        }
        Ok(())
    }
    /// Deterministic message identity scoped to the owning Session.
    pub fn message_id(&self) -> Result<crate::MessageId> {
        self.validate()?;
        crate::MessageId::new(format!(
            "command-{}-{}",
            self.invocation_sha256, self.ordinal
        ))
    }
}

/// Exact predecessor in either the process-local draft or durable control stream.
#[derive(schemars::JsonSchema, Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
#[derive(schemars::JsonSchema, Clone, Debug, Eq, PartialEq, Serialize)]
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
#[derive(schemars::JsonSchema, Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
        let mut hash = Sha256::new();
        hash.update(b"rsi-session-command-v1\0");
        serde_json::to_writer(&mut hash, self)
            .map_err(|error| SessionError::Encoding(error.to_string()))?;
        Ok(format!("{:x}", hash.finalize()))
    }
}

/// Arguments emitted when a text command accepts attached objects.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AttachedCommandArguments<'a> {
    /// Text following the command token.
    pub text: &'a str,
    /// Ordered external objects accompanying the text.
    pub attachments: Vec<CommandAttachment>,
}

/// A command can receive a file or image, with the same object identity as direct input.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CommandAttachment {
    /// Imported raster image.
    Image {
        /// Canonical Media object.
        media: rsi_media_protocol::MediaRef,
    },
    /// Verbatim file.
    File {
        /// Canonical file object.
        file: rsi_media_protocol::FileRef,
    },
}
impl From<CommandAttachment> for crate::AgentMessageContent {
    fn from(attachment: CommandAttachment) -> Self {
        match attachment {
            CommandAttachment::Image { media } => Self::Image {
                media,
                presentation: None,
            },
            CommandAttachment::File { file } => Self::File { file },
        }
    }
}

/// Composer input declaration, independent of structured execution arguments.
#[derive(schemars::JsonSchema, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ComposerCommandInput {
    /// Execute without changing unrelated editor content.
    None,
    /// Enter visible argument editing and wait for submission.
    Text {
        /// Bounded presentation hint; the command still validates its arguments.
        hint: String,
        /// Accept ordered file and image blocks alongside the argument text.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        attachments: bool,
    },
}

/// Bounded discovery metadata; selecting a command still uses its contribution identity.
#[derive(schemars::JsonSchema, Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCommandDescriptor {
    id: ContributionId,
    name: String,
    description: String,
    draft_safe: bool,
    composer_input: Option<ComposerCommandInput>,
}

impl SessionCommandDescriptor {
    /// Creates discoverable command metadata; the name omits the presentation slash.
    pub fn new(
        id: ContributionId,
        name: impl Into<String>,
        description: impl Into<String>,
        draft_safe: bool,
        composer_input: Option<ComposerCommandInput>,
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
        if let Some(ComposerCommandInput::Text { hint, .. }) = &composer_input {
            crate::validate_safe_diagnostic("command input hint", hint)?;
            if hint.len() > 256 {
                return Err(SessionError::Invalid(
                    "command input hint exceeds 256 bytes".into(),
                ));
            }
        }
        Ok(Self {
            id,
            name,
            description,
            draft_safe,
            composer_input,
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
    /// Returns composer input semantics, or none for structured-only commands.
    pub const fn composer_input(&self) -> Option<&ComposerCommandInput> {
        self.composer_input.as_ref()
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
            #[serde(deserialize_with = "Option::deserialize")]
            composer_input: Option<ComposerCommandInput>,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.id,
            wire.name,
            wire.description,
            wire.draft_safe,
            wire.composer_input,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// One bounded discovery snapshot tied to the caller-visible predecessor.
#[derive(schemars::JsonSchema, Clone, Debug, Eq, PartialEq, Serialize)]
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
