use serde::{Deserialize, Serialize};
use std::fmt;
/// Closed, redacted outcomes. External error strings and credentials never enter status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpError {
    /// Explicit configuration does not admit this operation.
    Disabled,
    /// Owner-local credential could not be resolved.
    CredentialUnavailable,
    /// Endpoint or protocol stream is no longer verified.
    Disconnected,
    /// Saved definitions differ from the currently verified catalog.
    CatalogChanged,
    /// Another bounded operation owns this endpoint.
    Busy,
    /// Response violates the negotiated finite protocol.
    Protocol,
    /// A complete frame, catalog, or result exceeds its bound.
    Capacity,
    /// The server returned a JSON-RPC error.
    RemoteError,
    /// The server implements no mutually supported modern protocol version.
    UnsupportedVersion,
    /// The operation requires a client capability this integration does not advertise.
    RequiredCapability,
    /// HTTP routing metadata did not match the server's current schema.
    HeaderMismatch,
    /// The server returned an unfinished multi round-trip operation; no retry was sent.
    InputRequired,
    /// The bounded operation deadline elapsed.
    Timeout,
    /// Owner or caller cancelled the operation.
    Cancelled,
    /// Exact selected Tool or resource is absent.
    NotFound,
    /// Configured process could not be admitted or confined.
    ProcessUnavailable,
}
impl fmt::Display for McpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Disabled => "MCP server is disabled",
            Self::CredentialUnavailable => "MCP credential is unavailable",
            Self::Disconnected => {
                "MCP server is disconnected; refresh its connection before retrying"
            }
            Self::CatalogChanged => {
                "MCP catalog changed; start a new Session to use its current definitions"
            }
            Self::Busy => "MCP server already has an operation in progress",
            Self::Protocol => "MCP server returned an invalid protocol response",
            Self::Capacity => "MCP operation exceeded its complete response or catalog limit",
            Self::RemoteError => "MCP server returned a protocol error",
            Self::UnsupportedVersion => "MCP server does not support a compatible protocol version",
            Self::RequiredCapability => "MCP operation requires an unavailable client capability",
            Self::HeaderMismatch => "MCP request headers do not match; refresh the server catalog",
            Self::InputRequired => "MCP operation requires additional input; no retry was sent",
            Self::Timeout => "MCP operation timed out; a started call was not replayed",
            Self::Cancelled => "MCP operation was cancelled; a started call was not replayed",
            Self::NotFound => "MCP Tool or resource is absent from the frozen catalog",
            Self::ProcessUnavailable => "MCP process could not be started",
        })
    }
}
impl std::error::Error for McpError {}

/// Actual redacted observation of one configured endpoint.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerStatus {
    /// Stable configured identity, without target URLs or launch paths.
    pub id: String,
    /// Transport category, without launch arguments or endpoint addresses.
    pub transport: McpTransportKind,
    /// Optional non-secret HTTP credential reference for an explicit setup action.
    pub credential: Option<rsi_credentials_protocol::CredentialRef>,
    /// Complete last-verified Tool name choices, without schemas or instructions.
    pub tools: Vec<McpToolChoice>,
    /// Explicit opt-in state.
    pub enabled: bool,
    /// Decimal epoch, avoiding JavaScript integer rounding.
    pub epoch: String,
    /// The last verified complete manifest, retained after disconnect.
    pub last_verified_sha256: Option<String>,
    /// Whether this exact epoch currently admits calls.
    pub ready: bool,
    /// Closed failure category, never a transport error string.
    pub error: Option<McpError>,
}
/// Redacted transport category.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTransportKind {
    /// Explicit HTTP/S endpoint.
    Http,
    /// Local-only configured process.
    Stdio,
}
/// A last-verified Tool name and its exact frozen selection flag.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpToolChoice {
    /// Original server Tool identity; never parsed from its public name.
    pub name: String,
    /// Selection recorded with this verified catalog.
    pub selected: bool,
}
/// Bounded current configuration/connection observation, containing no endpoint secrets or paths.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpStatus {
    /// Saved HTTP settings differ from the last applied configuration.
    pub settings_pending: bool,
    /// A complete current manifest is available to new Sessions.
    pub fresh_ready: bool,
    /// Closed reason why a fresh manifest is unavailable.
    pub fresh_error: Option<McpError>,
    /// Actual applied endpoint states, independent of saved configuration changes.
    pub servers: Vec<ServerStatus>,
}
impl McpStatus {
    /// Validates finite identities, counts and closed state relationships at the API boundary.
    pub fn validate(&self) -> super::Result<()> {
        if self.servers.len() > super::MAXIMUM_SERVERS
            || self.servers.windows(2).any(|pair| pair[0].id >= pair[1].id)
            || self.fresh_ready == self.fresh_error.is_some()
            || self.fresh_ready && self.settings_pending
        {
            return Err("Invalid MCP status".into());
        }
        for server in &self.servers {
            if let Some(reference) = &server.credential {
                reference
                    .validate()
                    .map_err(|_| "Invalid MCP credential reference")?;
                if reference.owner.as_str() != super::CREDENTIAL_OWNER
                    || server.transport != McpTransportKind::Http
                {
                    return Err("Invalid MCP credential status binding".into());
                }
            }
            if !super::name(&server.id, 64)
                || !server
                    .id
                    .bytes()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'.' | b'_' | b'-'))
                || server
                    .epoch
                    .parse::<u64>()
                    .ok()
                    .is_none_or(|epoch| epoch.to_string() != server.epoch)
                || server.ready == server.error.is_some()
                || server.ready
                    && (!server.enabled
                        || server.error.is_some()
                        || server.last_verified_sha256.is_none())
                || self.fresh_ready && server.enabled && !server.ready
                || server.tools.len() > super::MAXIMUM_TOOLS
                || server
                    .tools
                    .iter()
                    .any(|tool| !super::name(&tool.name, 256))
                || server
                    .tools
                    .iter()
                    .map(|tool| &tool.name)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != server.tools.len()
                || server.last_verified_sha256.as_ref().is_some_and(|value| {
                    value.len() != 64
                        || !value
                            .bytes()
                            .all(|ch| ch.is_ascii_digit() || (b'a'..=b'f').contains(&ch))
                })
            {
                return Err("Invalid MCP endpoint observation".into());
            }
        }
        Ok(())
    }
}
