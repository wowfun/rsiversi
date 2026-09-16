use super::{CREDENTIAL_OWNER, MAXIMUM_SERVERS, MAXIMUM_TOOLS, Result, digest, name};
use rsi_credentials_protocol::CredentialRef;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
/// Explicit settings; no servers are enabled by default.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {
    /// Complete configured server set.
    #[serde(default)]
    pub servers: Vec<ServerConfig>,
}
/// One explicit connection identity and selected raw Tool names.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Stable local namespace, independent of the server-reported name.
    pub id: String,
    /// Human configuration must explicitly opt in.
    #[serde(default)]
    pub enabled: bool,
    /// Exact raw Tool names; empty means expose no model Tools.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Explicit transport and credential references.
    pub transport: TransportConfig,
}
/// Non-secret configuration for an exact transport target.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TransportConfig {
    /// MCP Streamable HTTP with per-request credential resolution.
    StreamableHttp {
        /// Absolute HTTP/S URL without user information or fragments.
        url: String,
        /// Optional owner-local bearer token reference.
        #[serde(default)]
        credential: Option<CredentialRef>,
    },
    /// Local-only managed stdio process. No ambient executable search or environment.
    Stdio {
        /// Absolute executable.
        program: PathBuf,
        /// Explicit argv, excluding `argv[0]`.
        #[serde(default)]
        arguments: Vec<String>,
        /// Absolute working directory.
        cwd: PathBuf,
        /// Complete explicit child environment.
        #[serde(default)]
        environment: BTreeMap<String, EnvironmentValue>,
    },
}
/// Explicit literal value or owner-local secret reference for child environment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EnvironmentValue {
    /// Non-secret configuration text.
    Literal {
        /// Exact text.
        value: String,
    },
    /// Resolved only while connecting; never persisted as a value.
    Credential {
        /// Owner-local reference.
        reference: CredentialRef,
    },
}
fn credential(reference: &CredentialRef) -> Result<()> {
    reference
        .validate()
        .map_err(|_| "Invalid MCP credential reference")?;
    if reference.owner.as_str() != CREDENTIAL_OWNER {
        return Err("MCP credentials must use the rsi.mcp owner".into());
    }
    Ok(())
}
impl McpConfig {
    /// Checks cardinalities and all target inputs before connecting.
    pub fn validate(&self) -> Result<()> {
        if self.servers.len() > MAXIMUM_SERVERS {
            return Err("MCP configuration exceeds eight servers".into());
        }
        let mut ids = BTreeSet::new();
        for server in &self.servers {
            server.validate()?;
            if !ids.insert(&server.id) {
                return Err("Duplicate MCP server identity".into());
            }
        }
        if rsi_agent_session_protocol::DomainStateValue::encode(self).is_err() {
            return Err("MCP configuration exceeds 256 KiB".into());
        }
        Ok(())
    }
    /// Whether a remote replacement preserves every existing Local stdio entry
    /// and introduces no new one. HTTP entries remain subject to configuration grants.
    pub fn remote_replacement_of(&self, current: &Self) -> bool {
        let stdio = |config: &Self| {
            config
                .servers
                .iter()
                .filter(|server| matches!(server.transport, TransportConfig::Stdio { .. }))
                .map(|server| (server.id.clone(), server.clone()))
                .collect::<BTreeMap<_, _>>()
        };
        stdio(self) == stdio(current)
    }
}
impl ServerConfig {
    /// Validates complete explicit target configuration without acquiring authority.
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty()
            || self.id.len() > 64
            || !self
                .id
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'.' | b'_' | b'-'))
        {
            return Err("Invalid MCP server identity".into());
        }
        if self.tools.len() > MAXIMUM_TOOLS
            || self.tools.iter().any(|tool| !name(tool, 256))
            || self.tools.iter().collect::<BTreeSet<_>>().len() != self.tools.len()
        {
            return Err("Invalid or duplicate MCP Tool selection".into());
        }
        match &self.transport {
            TransportConfig::StreamableHttp {
                url,
                credential: reference,
            } => {
                if url.len() > 4096 {
                    return Err("MCP endpoint exceeds 4096 bytes".into());
                }
                let parsed = url::Url::parse(url).map_err(|_| "Invalid MCP endpoint")?;
                if !matches!(parsed.scheme(), "http" | "https")
                    || parsed.host_str().is_none()
                    || !parsed.username().is_empty()
                    || parsed.password().is_some()
                    || parsed.fragment().is_some()
                {
                    return Err(
                        "MCP endpoint must be HTTP/S without user information or a fragment".into(),
                    );
                }
                if let Some(reference) = reference {
                    credential(reference)?;
                    let loopback = match parsed.host() {
                        Some(url::Host::Domain("localhost")) => true,
                        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                        _ => false,
                    };
                    if parsed.scheme() != "https" && !loopback {
                        return Err(
                            "Credentialed MCP endpoints require HTTPS outside loopback".into()
                        );
                    }
                }
            }
            TransportConfig::Stdio {
                program,
                arguments,
                cwd,
                environment,
            } => {
                if !program.is_absolute()
                    || !cwd.is_absolute()
                    || program.as_os_str().len() > 4096
                    || cwd.as_os_str().len() > 4096
                    || program.as_os_str().as_encoded_bytes().contains(&0)
                    || cwd.as_os_str().as_encoded_bytes().contains(&0)
                {
                    return Err("MCP stdio requires absolute bounded program and cwd".into());
                }
                if arguments.len() > 256
                    || arguments.iter().any(|value| value.contains('\0'))
                    || arguments.iter().map(String::len).sum::<usize>() > 65536
                    || environment.len() > 64
                {
                    return Err("MCP stdio arguments or environment exceed limits".into());
                }
                let mut bytes = 0;
                for (key, value) in environment {
                    if key.is_empty() || key.len() > 256 || key.contains(['=', '\0']) {
                        return Err("Invalid MCP environment name".into());
                    }
                    match value {
                        EnvironmentValue::Literal { value } => {
                            if value.contains('\0') {
                                return Err("Invalid MCP environment value".into());
                            }
                            bytes += value.len();
                        }
                        EnvironmentValue::Credential { reference } => credential(reference)?,
                    }
                    if bytes > 65536 {
                        return Err("MCP literal environment exceeds 64 KiB".into());
                    }
                }
            }
        }
        Ok(())
    }
    /// Non-secret exact target/selection identity; credential values are excluded.
    pub fn target_sha256(&self) -> String {
        digest(self)
    }
}
