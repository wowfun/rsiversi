use crate::{Error, schema};
use serde_json::Value;
use std::sync::OnceLock;

macro_rules! check {
    ($name:ident, $dto:ty) => {
        pub(super) fn $name(value: &Value) -> Result<(), Error> {
            static VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();
            let validator = VALIDATOR.get_or_init(|| {
                let schema =
                    serde_json::to_value(schemars::schema_for!($dto)).expect("bundled ACP schema");
                jsonschema::validator_for(&schema).expect("valid bundled ACP schema")
            });
            if validator.is_valid(value) {
                Ok(())
            } else {
                Err(Error::Parameters)
            }
        }
    };
}
check!(setup, schema::NewSessionRequest);
check!(initialize, schema::InitializeRequest);
check!(prompt, schema::PromptRequest);
check!(permission, schema::RequestPermissionRequest);
check!(agent_initialize, schema::InitializeResponse);
check!(session_update, schema::SessionNotification);

/// Validates stable agent capabilities without the DTO's fallback-on-error behavior.
///
/// # Errors
/// Rejects malformed known fields and protocols other than stable version one.
pub fn validate_agent_initialize(value: &Value) -> Result<crate::observation::Capabilities, Error> {
    agent_initialize(value)?;
    if value.get("protocolVersion").and_then(Value::as_u64) != Some(1) {
        return Err(Error::Parameters);
    }
    let capabilities = &value["agentCapabilities"];
    let flag = |pointer: &str| {
        capabilities
            .pointer(pointer)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    let extension = |pointer: &str| capabilities.pointer(pointer).is_some_and(Value::is_object);
    Ok(crate::observation::Capabilities {
        load: flag("/loadSession"),
        resume: extension("/sessionCapabilities/resume"),
        close: extension("/sessionCapabilities/close"),
        image: flag("/promptCapabilities/image"),
        audio: flag("/promptCapabilities/audio"),
        embedded_context: flag("/promptCapabilities/embeddedContext"),
    })
}

/// Validates every known update field before an observation enters local history.
///
/// # Errors
/// Rejects malformed nested content, skipped collection entries and invalid identity.
pub fn validate_session_update(value: &Value) -> Result<(), Error> {
    crate::validate_session_id(value)?;
    session_update(value)
}

check!(new_result, schema::NewSessionResponse);
check!(load_result, schema::LoadSessionResponse);
check!(resume_result, schema::ResumeSessionResponse);
check!(close_result, schema::CloseSessionResponse);
check!(prompt_result, schema::PromptResponse);
check!(config_result, schema::SetSessionConfigOptionResponse);

/// Validates supported stable Session results before permissive DTO conversion.
///
/// # Errors
/// Rejects an unsupported method or malformed known result fields.
pub fn validate_session_result(method: &str, value: &Value) -> Result<(), Error> {
    match method {
        "session/new" => new_result(value),
        "session/load" => load_result(value),
        "session/resume" => resume_result(value),
        "session/close" => close_result(value),
        "session/prompt" => prompt_result(value),
        "session/set_config_option" => config_result(value),
        _ => Err(Error::Parameters),
    }
}
