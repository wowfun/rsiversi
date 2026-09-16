use super::{MAXIMUM_RESOURCES, MAXIMUM_SERVERS, MAXIMUM_TOOLS, Result, digest, name};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
/// Stable owning Agent Domain; codec identity is independent of connection epochs.
pub const MANIFEST_DOMAIN: &str = "rsi.mcp.manifest";
/// Negotiated protocol revisions implemented by this integration.
pub const PROTOCOL_VERSIONS: &[&str] = &[
    LATEST_PROTOCOL_VERSION,
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
];
/// Current stable stateless protocol revision.
pub const LATEST_PROTOCOL_VERSION: &str = "2026-07-28";
/// Full Tool metadata retained exactly, including unknown descriptive extensions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTool {
    /// Original server name, never recovered by parsing its public name.
    pub name: String,
    /// Optional display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// External descriptive text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Complete original JSON schema.
    pub input_schema: Value,
    /// Complete optional structured-output schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    /// Descriptive annotations, never authorization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Value>,
    /// Additional protocol metadata preserved without execution semantics.
    #[serde(flatten)]
    pub extensions: BTreeMap<String, Value>,
}
/// One declared resource; reading does not treat its URI as an HTTP URL.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpResource {
    /// Exact server resource identity.
    pub uri: String,
    /// Human display name.
    pub name: String,
    /// Optional attributed description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional advertised content type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Remaining descriptive metadata.
    #[serde(flatten)]
    pub extensions: BTreeMap<String, Value>,
}
/// Complete recorded Tool plus explicit selection and deterministic public identity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenTool {
    /// Original metadata and schema.
    pub definition: McpTool,
    /// Human-selected exposure in this generation.
    pub selected: bool,
    /// Stable bounded model-facing name.
    pub public_name: String,
}
/// Complete verified catalog for one exact configured target.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerManifest {
    /// Configured local identity.
    pub id: String,
    /// Exact non-secret target and selection fingerprint.
    pub target_sha256: String,
    /// Negotiated supported version.
    pub protocol_version: String,
    /// Complete server identification metadata.
    pub server_info: Value,
    /// Complete negotiated server capabilities.
    pub capabilities: Value,
    /// Attributed external instructions, exposed by explicit resource reads.
    pub instructions: Option<String>,
    /// Full discovered Tool set, including explicit selection flags.
    pub tools: Vec<FrozenTool>,
    /// Full listed resource set.
    pub resources: Vec<McpResource>,
}
/// One complete typed Domain value; never truncated or sharded.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpManifest {
    /// Sorted unique configured servers with verified catalogs.
    pub servers: Vec<ServerManifest>,
}
/// Derives a model-safe name without conflating different raw identities.
pub fn public_tool_name(server: &str, tool: &str) -> String {
    let joined = format!("mcp__{server}__{tool}");
    if joined.len() <= 64
        && joined
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'_' | b'-'))
    {
        return joined;
    }
    let mut normalized: String = joined
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .take(51)
        .collect();
    normalized.push('_');
    normalized.push_str(&digest(&(server, tool))[..12]);
    normalized
}
impl McpTool {
    /// Validates names and complete schema before registration.
    pub fn validate(&self) -> Result<()> {
        if !self.input_schema.is_object()
            || !name(&self.name, 256)
            || self.title.as_ref().is_some_and(|s| !name(s, 256))
            || self.description.as_ref().is_some_and(|s| s.len() > 4096)
            || self
                .output_schema
                .as_ref()
                .is_some_and(|schema| !schema.is_object())
            || self
                .annotations
                .as_ref()
                .is_some_and(|annotations| !annotations.is_object())
        {
            return Err("Invalid MCP Tool metadata".into());
        }
        rsi_tools_protocol::ToolDefinition::new(
            public_tool_name("validation", &self.name),
            self.description.clone().unwrap_or_default(),
            self.input_schema.clone(),
        )
        .map_err(|_| "Invalid MCP Tool input schema".to_owned())?;
        Ok(())
    }
}
impl ServerManifest {
    /// Checks complete metadata and deterministic Tool identities.
    pub fn validate(&self) -> Result<()> {
        if !name(&self.id, 64)
            || !self
                .id
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'.' | b'_' | b'-'))
            || self.target_sha256.len() != 64
            || !self
                .target_sha256
                .bytes()
                .all(|ch| ch.is_ascii_digit() || (b'a'..=b'f').contains(&ch))
            || !PROTOCOL_VERSIONS.contains(&self.protocol_version.as_str())
            || !self.server_info.is_object()
            || !self.capabilities.is_object()
            || self
                .instructions
                .as_ref()
                .is_some_and(|value| value.len() > 32768)
            || self.tools.len() > MAXIMUM_TOOLS
            || (self.resources.len() + usize::from(self.instructions.is_some())) > MAXIMUM_RESOURCES
        {
            return Err("Invalid MCP server manifest".into());
        }
        let mut tools = BTreeSet::new();
        for tool in &self.tools {
            tool.definition.validate()?;
            if !tools.insert(&tool.definition.name)
                || tool.public_name != public_tool_name(&self.id, &tool.definition.name)
            {
                return Err("Invalid MCP Tool identity".into());
            }
        }
        let mut resources = BTreeSet::new();
        for resource in &self.resources {
            if !name(&resource.uri, 4096)
                || !name(&resource.name, 256)
                || resource
                    .description
                    .as_ref()
                    .is_some_and(|v| v.len() > 4096)
                || resource.mime_type.as_ref().is_some_and(|v| !name(v, 128))
                || !resources.insert(&resource.uri)
            {
                return Err("Invalid MCP resource metadata".into());
            }
        }
        Ok(())
    }
    /// Complete verified content identity, independent of live epoch.
    pub fn sha256(&self) -> String {
        digest(self)
    }
}
impl McpManifest {
    /// Pure owner codec validation before Domain/Tool registration.
    pub fn validate(&self) -> Result<()> {
        if self.servers.len() > MAXIMUM_SERVERS
            || self.servers.windows(2).any(|pair| pair[0].id >= pair[1].id)
            || self
                .servers
                .iter()
                .map(|server| server.tools.len())
                .sum::<usize>()
                > MAXIMUM_TOOLS
            || self
                .servers
                .iter()
                .map(|server| server.resources.len() + usize::from(server.instructions.is_some()))
                .sum::<usize>()
                > MAXIMUM_RESOURCES
        {
            return Err("MCP manifest exceeds server, Tool or resource limits".into());
        }
        let selected = self
            .servers
            .iter()
            .flat_map(|server| &server.tools)
            .filter(|tool| tool.selected)
            .count();
        let reader = self
            .servers
            .iter()
            .any(|server| !server.resources.is_empty() || server.instructions.is_some());
        if selected + usize::from(reader) > rsi_tools_protocol::MAXIMUM_REGISTERED_TOOLS {
            return Err(
                "Selected MCP Tools and resource reader exceed the shared Tool limit".into(),
            );
        }
        let mut public_names = BTreeSet::new();
        for server in &self.servers {
            server.validate()?;
            for tool in &server.tools {
                if !public_names.insert(&tool.public_name) {
                    return Err("MCP public Tool identity collision".into());
                }
            }
        }
        rsi_agent_session_protocol::DomainStateValue::encode(self)
            .map_err(|_| "Complete MCP manifest exceeds the 256 KiB Domain limit".to_owned())?;
        Ok(())
    }
    /// Produces the complete typed initial Domain value for pre-seal composition.
    #[expect(
        clippy::missing_panics_doc,
        reason = "Only fixed validated constants and infallible JSON flag serialization are unwrapped."
    )]
    pub fn snapshot(&self) -> Result<rsi_agent_session_protocol::DomainSnapshot> {
        self.validate()?;
        Ok(rsi_agent_session_protocol::DomainSnapshot::new(
            rsi_agent_session_protocol::DomainIdentity::new(MANIFEST_DOMAIN, 1)
                .expect("static MCP domain"),
            rsi_agent_session_protocol::DomainStateValue::encode(self)
                .map_err(|_| "MCP manifest encoding failed")?,
        ))
    }
}
