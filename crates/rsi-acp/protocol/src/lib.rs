//! Bounded stable ACP wire input; transport and product state are separate owners.

#![deny(unsafe_code)]
#![warn(missing_docs)]

/// Bounded operator configuration for external Session startup.
pub mod configuration;
/// Local external-conversation identities and observed-history DTOs.
pub mod observation;
mod raw;
mod validation;
pub use agent_client_protocol_schema::v1 as schema;
pub use raw::{FrameDecoder, Message, RequestId, decode};
use serde_json::Value;
pub use validation::{validate_agent_initialize, validate_session_result, validate_session_update};

/// Maximum NDJSON record bytes excluding its delimiter.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// Maximum retained encoded payload in either independent connection direction.
pub const MAX_DIRECTION_BYTES: usize = 8 * 1024 * 1024;
/// Maximum simultaneously outstanding requests in one connection.
pub const MAX_PENDING: usize = 32;
/// Maximum encoded session MCP configuration.
pub const MAX_MCP_BYTES: usize = 256 * 1024;
/// Shared identity of the native delegation Tool and its navigation hints.
pub const EXTERNAL_AGENT_TOOL_NAME: &str = "external_agent";

/// Categorical errors that never include potentially secret wire data.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    /// A byte, count or depth limit was exceeded.
    #[error("ACP input exceeds its bound")]
    Limit,
    /// Invalid framing or JSON-RPC envelope.
    #[error("invalid ACP frame")]
    Frame,
    /// Unsupported or malformed ACP parameters.
    #[error("invalid ACP parameters")]
    Parameters,
}

/// Validates complete session setup before the schema's permissive decoding.
///
/// # Errors
/// Rejects unsupported transports, malformed or duplicate entries, and bounds.
pub fn validate_session_setup(params: &Value, existing: bool) -> Result<(), Error> {
    validation::setup(params)?;
    if params
        .get("additionalDirectories")
        .is_some_and(|directories| {
            directories
                .as_array()
                .is_none_or(|directories| !directories.is_empty())
        })
    {
        return Err(Error::Parameters);
    }
    let cwd = text(params.get("cwd"), 4096)?;
    if !std::path::Path::new(cwd).is_absolute() || cwd.contains('\0') {
        return Err(Error::Parameters);
    }
    if existing {
        text(params.get("sessionId"), 256)?;
    }
    let servers = params
        .get("mcpServers")
        .and_then(Value::as_array)
        .ok_or(Error::Parameters)?;
    if servers.len() > 8
        || serde_json::to_vec(servers)
            .map_err(|_| Error::Parameters)?
            .len()
            > MAX_MCP_BYTES
    {
        return Err(Error::Limit);
    }
    let mut names = std::collections::BTreeSet::new();
    for server in servers {
        let object = server.as_object().ok_or(Error::Parameters)?;
        if object.contains_key("type") {
            return Err(Error::Parameters);
        }
        let name = text(server.get("name"), 64)?;
        if !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
            || !names.insert(name)
        {
            return Err(Error::Parameters);
        }
        let command = text(server.get("command"), 4096)?;
        if !std::path::Path::new(command).is_absolute() || command.contains('\0') {
            return Err(Error::Parameters);
        }
        let args = server
            .get("args")
            .and_then(Value::as_array)
            .ok_or(Error::Parameters)?;
        if args.len() > 256 {
            return Err(Error::Limit);
        }
        for arg in args {
            if arg
                .as_str()
                .is_none_or(|arg| arg.len() > 16384 || arg.contains('\0'))
            {
                return Err(Error::Parameters);
            }
        }
        let env = server
            .get("env")
            .and_then(Value::as_array)
            .ok_or(Error::Parameters)?;
        if env.len() > 128 {
            return Err(Error::Limit);
        }
        let mut keys = std::collections::BTreeSet::new();
        for entry in env {
            let key = text(entry.get("name"), 256)?;
            let value = entry
                .get("value")
                .and_then(Value::as_str)
                .ok_or(Error::Parameters)?;
            if key.contains(['=', '\0'])
                || value.contains('\0')
                || value.len() > 16384
                || !keys.insert(key)
            {
                return Err(Error::Parameters);
            }
        }
    }
    Ok(())
}

/// Validates initialization fields which permissive schema decoding could default.
///
/// # Errors
/// Rejects malformed capability objects, flags and client identity fields.
pub fn validate_initialize(params: &Value) -> Result<(), Error> {
    validation::initialize(params)?;
    if params
        .get("protocolVersion")
        .and_then(Value::as_u64)
        .is_none_or(|version| version == 0 || version > u64::from(u16::MAX))
    {
        return Err(Error::Parameters);
    }
    if let Some(capabilities) = params.get("clientCapabilities") {
        if !capabilities.is_object() {
            return Err(Error::Parameters);
        }
        for pointer in [
            "/terminal",
            "/fs/readTextFile",
            "/fs/writeTextFile",
            "/auth/terminal",
        ] {
            if capabilities
                .pointer(pointer)
                .is_some_and(|flag| !flag.is_boolean())
            {
                return Err(Error::Parameters);
            }
        }
        for key in ["fs", "auth"] {
            if capabilities
                .get(key)
                .is_some_and(|value| !value.is_object())
            {
                return Err(Error::Parameters);
            }
        }
        for key in ["session", "elicitation"] {
            if capabilities
                .get(key)
                .is_some_and(|value| !value.is_null() && !value.is_object())
            {
                return Err(Error::Parameters);
            }
        }
    }
    if let Some(info) = params.get("clientInfo").filter(|info| !info.is_null()) {
        text(info.get("name"), 256)?;
        text(info.get("version"), 256)?;
    }
    Ok(())
}

/// Validates the identity in a close or cancel request before backend access.
///
/// # Errors
/// Rejects missing, empty and oversized Session IDs.
pub fn validate_session_id(params: &Value) -> Result<(), Error> {
    text(params.get("sessionId"), 256).map(|_| ())
}

/// Validates bounded optional cursor and absolute cwd list filters.
///
/// # Errors
/// Rejects malformed filters before any enumeration.
pub fn validate_list(params: &Value) -> Result<(), Error> {
    if let Some(cursor) = params.get("cursor").filter(|cursor| !cursor.is_null()) {
        text(Some(cursor), 2048)?;
    }
    if let Some(cwd) = params.get("cwd").filter(|cwd| !cwd.is_null()) {
        let cwd = text(Some(cwd), 4096)?;
        if !std::path::Path::new(cwd).is_absolute() || cwd.contains('\0') {
            return Err(Error::Parameters);
        }
    }
    Ok(())
}

/// Validates the supported prompt content before any submission.
///
/// # Errors
/// Rejects missing, skipped or unsupported content and oversized values.
pub fn validate_prompt(params: &Value) -> Result<(), Error> {
    validation::prompt(params)?;
    text(params.get("sessionId"), 256)?;
    let blocks = params
        .get("prompt")
        .and_then(Value::as_array)
        .ok_or(Error::Parameters)?;
    if blocks.is_empty() || blocks.len() > 256 {
        return Err(Error::Parameters);
    }
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                text(block.get("text"), MAX_FRAME_BYTES)?;
            }
            Some("resource_link") => {
                text(block.get("uri"), 4096)?;
                text(block.get("name"), 256)?;
            }
            _ => return Err(Error::Parameters),
        }
    }
    Ok(())
}

/// Checks the exact options before constructing a local permission interaction.
///
/// # Errors
/// Rejects duplicate IDs, unsupported kinds, missing fields or excess options.
pub fn validate_permission(params: &Value) -> Result<(), Error> {
    validation::permission(params)?;
    text(params.get("sessionId"), 256)?;
    text(
        params
            .get("toolCall")
            .and_then(|call| call.get("toolCallId")),
        256,
    )?;
    let options = params
        .get("options")
        .and_then(Value::as_array)
        .ok_or(Error::Parameters)?;
    if options.is_empty() || options.len() > 32 {
        return Err(Error::Parameters);
    }
    let mut ids = std::collections::BTreeSet::new();
    for option in options {
        if !ids.insert(text(option.get("optionId"), 256)?) {
            return Err(Error::Parameters);
        }
        text(option.get("name"), 1024)?;
        if !matches!(
            option.get("kind").and_then(Value::as_str),
            Some("allow_once" | "allow_always" | "reject_once" | "reject_always")
        ) {
            return Err(Error::Parameters);
        }
    }
    Ok(())
}

fn text(value: Option<&Value>, bound: usize) -> Result<&str, Error> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty() && text.len() <= bound)
        .ok_or(Error::Parameters)
}

/// External conversation capability, independent of endpoint launch authority.
pub mod service;
