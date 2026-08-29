//! Runtime-independent explicit command contracts.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_media_protocol::MediaRef;
use rsi_meta_contract::LocalContract;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Maximum command name bytes.
pub const MAXIMUM_COMMAND_NAME_BYTES: usize = 256;
/// Maximum command text bytes.
pub const MAXIMUM_COMMAND_TEXT_BYTES: usize = 4 * 1024 * 1024;
/// Maximum durable Media references in one command result.
pub const MAXIMUM_COMMAND_MEDIA_REFS: usize = 64;

/// Explicit command request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRequest {
    /// Exact registered command name.
    pub name: String,
    /// Opaque bounded UTF-8 argument text.
    pub text: String,
}

impl<'de> Deserialize<'de> for CommandRequest {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireRequest {
            name: String,
            text: String,
        }

        let wire = WireRequest::deserialize(deserializer)?;
        let request = Self {
            name: wire.name,
            text: wire.text,
        };
        request
            .validate()
            .map(|()| request)
            .map_err(serde::de::Error::custom)
    }
}

impl CommandRequest {
    /// Revalidates one explicit command request.
    pub fn validate(&self) -> Result<()> {
        validate_text("command name", &self.name, MAXIMUM_COMMAND_NAME_BYTES)?;
        validate_text("command text", &self.text, MAXIMUM_COMMAND_TEXT_BYTES)
    }
}

/// Explicit command result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandResult {
    /// Human-facing output text.
    pub text: String,
    /// Durable Media references.
    #[serde(default)]
    pub media: Vec<MediaRef>,
}

impl<'de> Deserialize<'de> for CommandResult {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireResult {
            text: String,
            #[serde(default)]
            media: Vec<MediaRef>,
        }
        let wire = WireResult::deserialize(deserializer)?;
        let result = Self {
            text: wire.text,
            media: wire.media,
        };
        result
            .validate()
            .map(|()| result)
            .map_err(serde::de::Error::custom)
    }
}

impl CommandResult {
    /// Revalidates one complete command result.
    pub fn validate(&self) -> Result<()> {
        validate_text("command result", &self.text, MAXIMUM_COMMAND_TEXT_BYTES)?;
        if self.media.len() > MAXIMUM_COMMAND_MEDIA_REFS {
            return Err(CommandError::InvalidInput(format!(
                "command result contains more than {MAXIMUM_COMMAND_MEDIA_REFS} Media references"
            )));
        }
        for media in &self.media {
            media
                .validate()
                .map_err(|error| CommandError::InvalidInput(error.to_string()))?;
        }
        Ok(())
    }
}

/// Trusted process-local command handler.
#[async_trait]
pub trait CommandHandler: fmt::Debug + Send + Sync + 'static {
    /// Executes one explicit request.
    async fn execute(&self, text: String, cancellation: CancellationToken)
    -> Result<CommandResult>;
}

/// One active command definition.
#[derive(Clone)]
pub struct CommandDefinition {
    /// Exact name.
    pub name: String,
    /// Bounded description.
    pub description: String,
    /// Trusted handler.
    pub handler: Arc<dyn CommandHandler>,
}

impl CommandDefinition {
    /// Revalidates one trusted process-local registration declaration.
    pub fn validate(&self) -> Result<()> {
        validate_text("command name", &self.name, MAXIMUM_COMMAND_NAME_BYTES)?;
        validate_text(
            "command description",
            &self.description,
            MAXIMUM_COMMAND_TEXT_BYTES,
        )
    }
}

impl fmt::Debug for CommandDefinition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CommandDefinition")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("handler", &"<command handler>")
            .finish()
    }
}

/// Schema projection without executable code.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandDescriptor {
    /// Exact name.
    pub name: String,
    /// Human-facing description.
    pub description: String,
}

impl<'de> Deserialize<'de> for CommandDescriptor {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireDescriptor {
            name: String,
            description: String,
        }

        let wire = WireDescriptor::deserialize(deserializer)?;
        let descriptor = Self {
            name: wire.name,
            description: wire.description,
        };
        descriptor
            .validate()
            .map(|()| descriptor)
            .map_err(serde::de::Error::custom)
    }
}

impl CommandDescriptor {
    /// Revalidates one code-free command projection.
    pub fn validate(&self) -> Result<()> {
        validate_text("command name", &self.name, MAXIMUM_COMMAND_NAME_BYTES)?;
        validate_text(
            "command description",
            &self.description,
            MAXIMUM_COMMAND_TEXT_BYTES,
        )
    }
}

fn validate_text(kind: &str, value: &str, maximum: usize) -> Result<()> {
    if value.is_empty() || value.len() > maximum {
        return Err(CommandError::InvalidInput(format!(
            "{kind} must be within 1..={maximum} bytes"
        )));
    }
    Ok(())
}

/// Closed command failure taxonomy.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CommandError {
    /// Malformed or out-of-bounds input/output.
    #[error("invalid command value: {0}")]
    InvalidInput(String),
    /// Duplicate registration.
    #[error("command `{0}` is already registered")]
    Duplicate(String),
    /// Unknown exact name.
    #[error("command `{0}` is not registered")]
    Unknown(String),
    /// Cooperative cancellation.
    #[error("command was cancelled")]
    Cancelled,
    /// Handler failure.
    #[error("command failed: {0}")]
    Execution(String),
}

/// Command result.
pub type Result<T> = std::result::Result<T, CommandError>;

/// Exact-name explicit command registry.
#[async_trait]
pub trait CommandRuntime: fmt::Debug + Send + Sync + 'static {
    /// Registers one command until the lease drops.
    fn register(&self, definition: CommandDefinition) -> Result<CommandLease>;
    /// Returns deterministic active descriptors.
    fn descriptors(&self) -> Vec<CommandDescriptor>;
    /// Executes one explicit request.
    async fn execute(
        &self,
        request: CommandRequest,
        cancellation: CancellationToken,
    ) -> Result<CommandResult>;
}

/// Nominal Local contract for [`CommandRuntime`].
#[derive(Debug)]
pub struct CommandRuntimeContract;

impl LocalContract for CommandRuntimeContract {
    const KEY: &'static str = "rsi.commands";
    type Service = dyn CommandRuntime;
}

/// Opaque command registration lease.
pub struct CommandLease {
    cleanup: Option<Box<dyn FnOnce() + Send + Sync + 'static>>,
}

impl CommandLease {
    /// Creates a lease from one unregister action.
    pub fn new(cleanup: impl FnOnce() + Send + Sync + 'static) -> Self {
        Self {
            cleanup: Some(Box::new(cleanup)),
        }
    }
}

impl fmt::Debug for CommandLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CommandLease(..)")
    }
}

impl Drop for CommandLease {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup();
        }
    }
}
