//! Private mirror of the version-zero native plugin JSON frame contract.
//!
//! The public composition interface does not expose ABI frames. Keeping this
//! DTO private lets the host validate every plugin byte sequence while the
//! independently published fixture contract remains the wire oracle.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::protocol::MAX_WIRE_ID_CHARACTERS;
use crate::protocol::{CommandOutcome, CommandOutcomeEnvelope};
use crate::{HostError, Result};

pub(crate) const PROTOCOL: &str = "rsi-meta.plugin";
pub(crate) const VERSION: u32 = 0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct PluginFrame {
    pub protocol: String,
    pub version: u32,
    #[serde(flatten)]
    pub body: PluginFrameBody,
}

impl PluginFrame {
    pub(crate) fn new(body: PluginFrameBody) -> Self {
        Self {
            protocol: PROTOCOL.to_owned(),
            version: VERSION,
            body,
        }
    }

    pub(crate) fn lifecycle(phase: LifecyclePhase, generation: u64, config: Option<Value>) -> Self {
        Self::new(PluginFrameBody::Lifecycle {
            phase,
            generation,
            config,
        })
    }

    pub(crate) fn service_request(
        request_id: impl Into<String>,
        service: impl Into<String>,
        operation: impl Into<String>,
        payload: Value,
    ) -> Self {
        Self::new(PluginFrameBody::ServiceRequest {
            request_id: request_id.into(),
            service: service.into(),
            operation: operation.into(),
            payload,
        })
    }

    pub(crate) fn service_event(
        request_id: Option<String>,
        service: impl Into<String>,
        event: impl Into<String>,
        payload: Value,
    ) -> Self {
        Self::new(PluginFrameBody::ServiceEvent {
            request_id,
            service: service.into(),
            event: event.into(),
            payload,
        })
    }

    pub(crate) fn durable_command_unavailable(command_id: String) -> Self {
        Self::service_event(
            Some(command_id),
            "control.apply-manifest",
            "failed",
            serde_json::json!({"code": "command_unavailable_during_lifecycle"}),
        )
    }

    pub(crate) fn durable_command_result(
        command_id: String,
        result: crate::Result<CommandOutcomeEnvelope>,
    ) -> Self {
        let (event, payload) = match result {
            Ok(outcome) => match outcome.payload {
                CommandOutcome::Applied { .. } => ("applied", serde_json::json!({})),
                CommandOutcome::NoChange { .. } => ("unchanged", serde_json::json!({})),
                CommandOutcome::RestartRequired { packages, .. } => (
                    "restart_required",
                    serde_json::json!({"packages": packages}),
                ),
                CommandOutcome::Rejected { code, message } => (
                    "rejected",
                    serde_json::json!({"code": code, "message": message}),
                ),
                _ => (
                    "failed",
                    serde_json::json!({"code": "invalid_command_outcome"}),
                ),
            },
            Err(error) => (
                "failed",
                serde_json::json!({"code": "host_error", "message": error.to_string()}),
            ),
        };
        Self::service_event(Some(command_id), "control.apply-manifest", event, payload)
    }

    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        let frame: Self = serde_json::from_slice(bytes)?;
        if frame.protocol != PROTOCOL || frame.version != VERSION {
            return Err(HostError::UnsupportedProtocol {
                protocol: frame.protocol,
                version: frame.version,
            });
        }
        frame.body.validate()?;
        Ok(frame)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum PluginFrameBody {
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
        command: DurablePluginCommand,
    },
}

impl PluginFrameBody {
    fn validate(&self) -> Result<()> {
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
                require_bounded("service request id", request_id)?;
                require_bounded("service request contract", service)?;
                require_bounded("service request operation", operation)?;
            }
            Self::ServiceEvent {
                request_id,
                service,
                event,
                ..
            } => {
                if let Some(request_id) = request_id {
                    require_bounded("service event request id", request_id)?;
                }
                require_bounded("service event contract", service)?;
                require_bounded("service event name", event)?;
            }
            Self::DurableCommand { command_id, .. } => {
                require_bounded("durable command id", command_id)?;
            }
        }
        Ok(())
    }
}

fn validate_prepare_failure(config: Option<&Value>) -> Result<()> {
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

fn require_nonempty(name: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(invalid_frame(format!("{name} must not be empty")));
    }
    Ok(())
}

fn require_bounded(name: &str, value: &str) -> Result<()> {
    require_nonempty(name, value)?;
    if value.chars().count() > MAX_WIRE_ID_CHARACTERS {
        return Err(invalid_frame(format!(
            "{name} exceeds {MAX_WIRE_ID_CHARACTERS} characters"
        )));
    }
    Ok(())
}

fn invalid_frame(message: impl Into<String>) -> HostError {
    HostError::InvalidEnvelope(message.into())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LifecyclePhase {
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
pub(crate) enum DurablePluginCommand {
    ApplyManifestPath {
        manifest_path: PathBuf,
        lock_path: PathBuf,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_enforces_the_published_frame_schema_basics() {
        let zero_generation = br#"{
          "protocol":"rsi-meta.plugin","version":0,"kind":"lifecycle",
          "phase":"prepared","generation":0
        }"#;
        assert!(PluginFrame::decode(zero_generation).is_err());

        let unknown_field = br#"{
          "protocol":"rsi-meta.plugin","version":0,"kind":"service_event",
          "service":"fixture.echo","event":"end","payload":null,"surprise":true
        }"#;
        assert!(PluginFrame::decode(unknown_field).is_err());

        let empty_identifier = br#"{
          "protocol":"rsi-meta.plugin","version":0,"kind":"service_request",
          "request_id":"","service":"fixture.echo","operation":"open","payload":{}
        }"#;
        assert!(PluginFrame::decode(empty_identifier).is_err());

        let oversized_durable_id = format!(
            r#"{{
              "protocol":"rsi-meta.plugin","version":0,"kind":"durable_command",
              "command_id":"{}","command":{{"type":"apply_manifest_path","manifest_path":"m","lock_path":"l"}}
            }}"#,
            "x".repeat(MAX_WIRE_ID_CHARACTERS + 1)
        );
        assert!(PluginFrame::decode(oversized_durable_id.as_bytes()).is_err());

        let oversized_service_id = format!(
            r#"{{
              "protocol":"rsi-meta.plugin","version":0,"kind":"service_request",
              "request_id":"request","service":"{}","operation":"open","payload":{{}}
            }}"#,
            "s".repeat(MAX_WIRE_ID_CHARACTERS + 1)
        );
        assert!(PluginFrame::decode(oversized_service_id.as_bytes()).is_err());

        let unknown_command_field = br#"{
          "protocol":"rsi-meta.plugin","version":0,"kind":"durable_command",
          "command_id":"apply-1","command":{"type":"apply_manifest_path","manifest_path":"m","lock_path":"l","surprise":true}
        }"#;
        assert!(PluginFrame::decode(unknown_command_field).is_err());
    }
}
