use crate::{
    error::{McpError, Result},
    transport::Connection,
};
use rsi_mcp_protocol::{
    FrozenTool, MAXIMUM_RESOURCES, MAXIMUM_TOOLS, McpManifest, McpTool, ServerConfig,
    ServerManifest, public_tool_name,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::BTreeSet;
async fn list<T: DeserializeOwned>(
    connection: &Connection,
    method: &str,
    field: &str,
    maximum: usize,
) -> Result<Vec<T>> {
    let mut items = Vec::new();
    let mut cursor = None;
    let mut seen = BTreeSet::new();
    let mut bytes = 0usize;
    for _ in 0..16 {
        let response = connection
            .request(
                method,
                cursor.map_or_else(|| json!({}), |cursor| json!({"cursor":cursor})),
            )
            .await?;
        // The complete discovery budget applies across pages, including metadata and cursors.
        let encoded = rsi_agent_session_protocol::DomainStateValue::encode(&response)
            .map_err(|_| McpError::Capacity)?;
        bytes = bytes
            .checked_add(encoded.encoded_len())
            .ok_or(McpError::Capacity)?;
        if bytes > rsi_agent_session_protocol::MAXIMUM_DOMAIN_STATE_BYTES {
            return Err(McpError::Capacity);
        }
        let entries = response
            .get(field)
            .and_then(Value::as_array)
            .ok_or(McpError::Protocol)?;
        if entries.len() > maximum.saturating_sub(items.len()) {
            return Err(McpError::Capacity);
        }
        for entry in entries {
            items.push(serde_json::from_value(entry.clone()).map_err(|_| McpError::Protocol)?);
        }
        match response.get("nextCursor") {
            None => return Ok(items),
            Some(Value::String(next))
                if !next.is_empty() && next.len() <= 4096 && seen.insert(next.clone()) =>
            {
                cursor = Some(next.clone());
            }
            _ => return Err(McpError::Protocol),
        }
    }
    Err(McpError::Capacity)
}
pub(crate) async fn discover(
    connection: &Connection,
    config: &ServerConfig,
    legacy: bool,
) -> Result<ServerManifest> {
    let (version, response) = connection.handshake(legacy).await?;
    let capabilities = response
        .get("capabilities")
        .filter(|caps| caps.is_object())
        .ok_or(McpError::Protocol)?
        .clone();
    let info = if connection.modern() {
        response
            .pointer("/_meta/io.modelcontextprotocol~1serverInfo")
            .cloned()
            .unwrap_or_else(|| json!({}))
    } else {
        response
            .get("serverInfo")
            .cloned()
            .ok_or(McpError::Protocol)?
    };
    if !info.is_object()
        || ["tools", "resources"].iter().any(|key| {
            capabilities
                .get(key)
                .is_some_and(|value| !value.is_object())
        })
    {
        return Err(McpError::Protocol);
    }
    let instructions = match response.get("instructions") {
        None => None,
        Some(Value::String(text)) if text.len() <= 32768 => Some(text.clone()),
        _ => return Err(McpError::Protocol),
    };
    connection.subscribe(&capabilities).await?;
    let definitions: Vec<McpTool> = if capabilities.get("tools").is_some() {
        list(connection, "tools/list", "tools", MAXIMUM_TOOLS).await?
    } else {
        vec![]
    };
    connection.configure_tools(&definitions)?;
    if config
        .tools
        .iter()
        .any(|selected| !definitions.iter().any(|tool| &tool.name == selected))
    {
        return Err(McpError::NotFound);
    }
    let tools = definitions
        .into_iter()
        .map(|definition| FrozenTool {
            selected: config.tools.contains(&definition.name),
            public_name: public_tool_name(&config.id, &definition.name),
            definition,
        })
        .collect();
    let resources = if capabilities.get("resources").is_some() {
        list(connection, "resources/list", "resources", MAXIMUM_RESOURCES).await?
    } else {
        vec![]
    };
    let manifest = ServerManifest {
        id: config.id.clone(),
        target_sha256: config.target_sha256(),
        protocol_version: version,
        server_info: info,
        capabilities,
        instructions,
        tools,
        resources,
    };
    manifest.validate().map_err(|_| McpError::Protocol)?;
    McpManifest {
        servers: vec![manifest.clone()],
    }
    .validate()
    .map_err(|_| McpError::Capacity)?;
    if !connection.valid() {
        return Err(McpError::Disconnected);
    }
    Ok(manifest)
}
