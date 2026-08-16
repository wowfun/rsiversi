//! Versioned JSON frames shared by all trusted plugin fixtures.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const PROTOCOL: &str = "rsi-meta.plugin";
pub const VERSION: u32 = 0;

pub const OP_OPEN: &str = "open";
pub const OP_DATA: &str = "data";
pub const OP_CREDIT: &str = "credit";
pub const OP_HALF_CLOSE: &str = "half_close";
pub const OP_CANCEL: &str = "cancel";

pub const EVENT_DATA: &str = "data";
pub const EVENT_CREDIT: &str = "credit";
pub const EVENT_END: &str = "end";
pub const EVENT_CANCEL: &str = "cancel";

pub const RUNTIME_TICK_SERVICE: &str = "runtime.tick";
pub const RUNTIME_TICK_EVENT: &str = "tick";

pub const STATE_OP_GET: &str = "get";
pub const STATE_OP_COMPARE_AND_SWAP: &str = "compare_and_swap";
pub const STATE_OP_DELETE: &str = "delete";

pub const STATE_EVENT_VALUE: &str = "value";
pub const STATE_EVENT_APPLIED: &str = "applied";
pub const STATE_EVENT_CONFLICT: &str = "conflict";
pub const STATE_EVENT_DELETED: &str = "deleted";
const MAX_DURABLE_COMMAND_ID_CHARACTERS: usize = 255;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub protocol: String,
    pub version: u32,
    #[serde(flatten)]
    pub body: FrameBody,
}

impl Frame {
    pub fn new(body: FrameBody) -> Self {
        Self {
            protocol: PROTOCOL.to_owned(),
            version: VERSION,
            body,
        }
    }

    pub fn lifecycle(phase: LifecyclePhase, generation: u64, config: Option<Value>) -> Self {
        Self::new(FrameBody::Lifecycle {
            phase,
            generation,
            config,
        })
    }

    pub fn service_request(
        request_id: impl Into<String>,
        service: impl Into<String>,
        operation: impl Into<String>,
        payload: Value,
    ) -> Self {
        Self::new(FrameBody::ServiceRequest {
            request_id: request_id.into(),
            service: service.into(),
            operation: operation.into(),
            payload,
        })
    }

    pub fn service_event(
        request_id: Option<String>,
        service: impl Into<String>,
        event: impl Into<String>,
        payload: Value,
    ) -> Self {
        Self::new(FrameBody::ServiceEvent {
            request_id,
            service: service.into(),
            event: event.into(),
            payload,
        })
    }

    pub fn durable_command(command_id: impl Into<String>, command: DurableCommand) -> Self {
        Self::new(FrameBody::DurableCommand {
            command_id: command_id.into(),
            command,
        })
    }

    /// Encodes this frame as its bounded JSON wire representation.
    ///
    /// # Errors
    ///
    /// Returns an error if JSON serialization fails.
    pub fn encode(&self) -> Result<Vec<u8>, FrameError> {
        serde_json::to_vec(self).map_err(FrameError::Json)
    }

    /// Decodes and validates a JSON frame.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid JSON or an unsupported protocol/version.
    pub fn decode(bytes: &[u8]) -> Result<Self, FrameError> {
        let frame: Self = serde_json::from_slice(bytes).map_err(FrameError::Json)?;
        if frame.protocol != PROTOCOL {
            return Err(FrameError::UnsupportedProtocol {
                found: frame.protocol,
            });
        }
        if frame.version != VERSION {
            return Err(FrameError::UnsupportedVersion {
                found: frame.version,
            });
        }
        frame.body.validate()?;
        Ok(frame)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FrameBody {
    Lifecycle {
        phase: LifecyclePhase,
        generation: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config: Option<Value>,
    },
    ServiceRequest {
        request_id: String,
        service: String,
        operation: String,
        payload: Value,
    },
    ServiceEvent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        service: String,
        event: String,
        payload: Value,
    },
    DurableCommand {
        command_id: String,
        command: DurableCommand,
    },
}

impl FrameBody {
    fn validate(&self) -> Result<(), FrameError> {
        match self {
            Self::Lifecycle {
                phase,
                generation,
                config,
            } => {
                if *generation == 0 {
                    return Err(invalid_frame("lifecycle generation must be at least one"));
                }
                match phase {
                    LifecyclePhase::Prepare => {}
                    LifecyclePhase::PrepareFailed => validate_prepare_failure(config.as_ref())?,
                    LifecyclePhase::Prepared
                    | LifecyclePhase::Abort
                    | LifecyclePhase::Committed
                    | LifecyclePhase::Retire
                    | LifecyclePhase::Retired => {
                        if config.is_some() {
                            return Err(invalid_frame(
                                "this lifecycle phase must not carry config",
                            ));
                        }
                    }
                }
            }
            Self::ServiceRequest {
                request_id,
                service,
                operation,
                ..
            } => {
                require_nonempty("service request id", request_id)?;
                require_nonempty("service request contract", service)?;
                require_nonempty("service request operation", operation)?;
            }
            Self::ServiceEvent {
                request_id,
                service,
                event,
                ..
            } => {
                if let Some(request_id) = request_id {
                    require_nonempty("service event request id", request_id)?;
                }
                require_nonempty("service event contract", service)?;
                require_nonempty("service event name", event)?;
            }
            Self::DurableCommand { command_id, .. } => {
                require_bounded_durable_command_id(command_id)?;
            }
        }
        Ok(())
    }
}

fn validate_prepare_failure(config: Option<&Value>) -> Result<(), FrameError> {
    let object = config
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_frame("prepare_failed requires an object config"))?;
    if object.keys().any(|key| key != "code" && key != "message") {
        return Err(invalid_frame(
            "prepare_failed config contains an unknown field",
        ));
    }
    let code = object
        .get("code")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_frame("prepare_failed config requires a string code"))?;
    if code.is_empty()
        || code.len() > 64
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(invalid_frame(
            "prepare_failed code is outside the v0 schema",
        ));
    }
    if let Some(message) = object.get("message") {
        let message = message
            .as_str()
            .ok_or_else(|| invalid_frame("prepare_failed message must be a string"))?;
        if message.chars().count() > 256
            || message
                .chars()
                .any(|character| character <= '\u{001f}' || character == '\u{007f}')
        {
            return Err(invalid_frame(
                "prepare_failed message is outside the v0 schema",
            ));
        }
    }
    Ok(())
}

fn require_nonempty(name: &str, value: &str) -> Result<(), FrameError> {
    if value.is_empty() {
        return Err(invalid_frame(format!("{name} must not be empty")));
    }
    Ok(())
}

fn require_bounded_durable_command_id(value: &str) -> Result<(), FrameError> {
    require_nonempty("durable command id", value)?;
    if value.chars().count() > MAX_DURABLE_COMMAND_ID_CHARACTERS {
        return Err(invalid_frame(format!(
            "durable command id exceeds {MAX_DURABLE_COMMAND_ID_CHARACTERS} characters"
        )));
    }
    Ok(())
}

fn invalid_frame(message: impl Into<String>) -> FrameError {
    FrameError::InvalidEnvelope(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_commands_match_the_published_identifier_and_field_bounds() {
        let oversized = serde_json::json!({
            "protocol": PROTOCOL,
            "version": VERSION,
            "kind": "durable_command",
            "command_id": "x".repeat(256),
            "command": {
                "type": "apply_manifest_path",
                "manifest_path": "m",
                "lock_path": "l"
            }
        });
        assert!(Frame::decode(oversized.to_string().as_bytes()).is_err());

        let unknown = serde_json::json!({
            "protocol": PROTOCOL,
            "version": VERSION,
            "kind": "durable_command",
            "command_id": "apply-1",
            "command": {
                "type": "apply_manifest_path",
                "manifest_path": "m",
                "lock_path": "l",
                "surprise": true
            }
        });
        assert!(Frame::decode(unknown.to_string().as_bytes()).is_err());
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecyclePhase {
    Prepare,
    Prepared,
    PrepareFailed,
    Abort,
    Committed,
    Retire,
    Retired,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum DurableCommand {
    ApplyManifestPath {
        manifest_path: PathBuf,
        lock_path: PathBuf,
    },
}

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("invalid plugin JSON frame: {0}")]
    Json(#[source] serde_json::Error),
    #[error("unsupported plugin frame protocol `{found}`")]
    UnsupportedProtocol { found: String },
    #[error("unsupported plugin frame version {found}")]
    UnsupportedVersion { found: u32 },
    #[error("invalid plugin frame: {0}")]
    InvalidEnvelope(String),
}
