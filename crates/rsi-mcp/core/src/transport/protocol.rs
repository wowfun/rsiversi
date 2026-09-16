use super::{Connection, State, Transport};
use crate::error::{McpError, Result};
use rsi_mcp_protocol::{LATEST_PROTOCOL_VERSION, McpTool, PROTOCOL_VERSIONS};
use serde_json::{Value, json};

impl State {
    pub(super) fn modern(&self) -> bool {
        *self.version.lock().expect("MCP version poisoned") == LATEST_PROTOCOL_VERSION
    }
    pub(super) fn set_version(&self, version: &str) -> Result<()> {
        let version = PROTOCOL_VERSIONS
            .iter()
            .find(|&&v| v == version)
            .ok_or(McpError::UnsupportedVersion)?;
        *self.version.lock().expect("MCP version poisoned") = version;
        Ok(())
    }
    pub(super) fn request_meta(&self, params: &mut Value) -> Result<()> {
        if self.modern() {
            params.as_object_mut().ok_or(McpError::Protocol)?.insert("_meta".into(), json!({
                "io.modelcontextprotocol/protocolVersion":LATEST_PROTOCOL_VERSION,
                "io.modelcontextprotocol/clientCapabilities":{},
                "io.modelcontextprotocol/clientInfo":{"name":"rsiversi","version":env!("CARGO_PKG_VERSION")}
            }));
        }
        Ok(())
    }
    pub(super) fn complete_result(&self, method: &str, result: Value) -> Result<Value> {
        match result.get("resultType").and_then(Value::as_str) {
            Some("complete") => {}
            None if !self.modern() && result.get("resultType").is_none() => {}
            Some("input_required")
                if self.modern()
                    && matches!(method, "tools/call" | "resources/read" | "prompts/get") =>
            {
                let inputs = result.get("inputRequests");
                let state = result.get("requestState");
                if (inputs.is_none() && state.is_none())
                    || inputs.is_some_and(|v| !v.is_object())
                    || state.is_some_and(|v| !v.is_string())
                {
                    return Err(McpError::Protocol);
                }
                // No input-producing capability was advertised; never synthesize replies.
                return Err(
                    if inputs
                        .and_then(Value::as_object)
                        .is_some_and(|v| !v.is_empty())
                    {
                        McpError::RequiredCapability
                    } else {
                        McpError::InputRequired
                    },
                );
            }
            _ => return Err(McpError::Protocol),
        }
        if self.modern()
            && matches!(
                method,
                "server/discover"
                    | "tools/list"
                    | "resources/list"
                    | "resources/read"
                    | "resources/templates/list"
                    | "prompts/list"
            )
            && (result
                .get("ttlMs")
                .and_then(Value::as_f64)
                .is_none_or(|v| !v.is_finite() || v < 0.0)
                || !matches!(
                    result.get("cacheScope").and_then(Value::as_str),
                    Some("public" | "private")
                ))
        {
            return Err(McpError::Protocol);
        }
        Ok(result)
    }
}

impl Connection {
    pub(crate) fn silent_probe(&self) -> bool {
        self.silent_probe.load(std::sync::atomic::Ordering::Acquire)
    }
    pub(crate) fn modern(&self) -> bool {
        self.state.modern()
    }
    pub(crate) async fn handshake(&self, legacy: bool) -> Result<(String, Value)> {
        if !legacy {
            match self.request("server/discover", json!({})).await {
                Ok(response) => {
                    let supported = response
                        .get("supportedVersions")
                        .and_then(Value::as_array)
                        .ok_or(McpError::Protocol)?;
                    if supported.is_empty()
                        || supported.len() > 32
                        || supported
                            .iter()
                            .any(|v| v.as_str().is_none_or(|s| s.len() > 128))
                    {
                        return Err(McpError::Protocol);
                    }
                    if !supported.iter().any(|v| v == LATEST_PROTOCOL_VERSION) {
                        return Err(McpError::UnsupportedVersion);
                    }
                    return Ok((LATEST_PROTOCOL_VERSION.into(), response));
                }
                // Era detection is confined to the side-effect-free probe.
                Err(McpError::RemoteError) => {}
                Err(error) => {
                    if error == McpError::Timeout {
                        self.silent_probe
                            .store(true, std::sync::atomic::Ordering::Release);
                    }
                    return Err(error);
                }
            }
        }
        self.state.set_version(PROTOCOL_VERSIONS[1])?;
        let response = self.request("initialize", json!({"protocolVersion":PROTOCOL_VERSIONS[1],"capabilities":{},"clientInfo":{"name":"rsiversi","version":env!("CARGO_PKG_VERSION")}})).await?;
        let version = response
            .get("protocolVersion")
            .and_then(Value::as_str)
            .filter(|version| PROTOCOL_VERSIONS[1..].contains(version))
            .ok_or(McpError::UnsupportedVersion)?
            .to_owned();
        self.initialized(&version).await?;
        Ok((version, response))
    }
    pub(crate) fn configure_tools(&self, tools: &[McpTool]) -> Result<()> {
        if self.modern() && matches!(self.transport, Transport::Http(_)) {
            let mut parameters = self
                .parameters
                .lock()
                .expect("MCP parameter headers poisoned");
            for tool in tools {
                parameters.insert(
                    tool.name.clone(),
                    rsi_mcp_protocol::http_parameters(&tool.input_schema)
                        .map_err(|_| McpError::Protocol)?,
                );
            }
        }
        Ok(())
    }
    pub(super) fn parameter_headers(
        &self,
        method: &str,
        params: &Value,
    ) -> Result<Vec<(String, String)>> {
        let mut headers = Vec::new();
        if method == "tools/call" && self.modern() {
            let parameters = self
                .parameters
                .lock()
                .expect("MCP parameter headers poisoned");
            if let Some(parameters) = params
                .get("name")
                .and_then(Value::as_str)
                .and_then(|name| parameters.get(name))
            {
                let arguments = params.get("arguments").ok_or(McpError::Protocol)?;
                let mut bytes = 0usize;
                for parameter in parameters {
                    if let Some((name, value)) = parameter
                        .project(arguments)
                        .map_err(|_| McpError::Protocol)?
                    {
                        bytes += name.len() + value.len();
                        if bytes > rsi_mcp_protocol::MAXIMUM_HTTP_PARAMETER_BYTES {
                            return Err(McpError::Capacity);
                        }
                        headers.push((name, value));
                    }
                }
            }
        }
        Ok(headers)
    }
}
