use super::{PluginsFeature, Result};
use rsi_configuration_api::mcp_ssh as wire;
use serde::{Deserialize, Serialize};

/// Explicit exact-target stdio administration. No credential values are resolved.
#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpSshCommand {
    /// Observe the selected server and its configuration revision.
    Read {
        /// Exact Host, execution target and server.
        target: wire::Target,
    },
    /// Persist a complete explicit configuration.
    Put {
        /// Exact selection, document revision and optional configuration.
        change: wire::Change,
    },
    /// Remove one exact server at the displayed revision.
    Remove {
        /// Exact selection, document revision and optional configuration.
        change: wire::Change,
    },
    /// Connect explicitly under separate current Use authority.
    Refresh {
        /// Exact selection, document revision and optional configuration.
        change: wire::Change,
    },
}
impl std::fmt::Debug for McpSshCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("McpSshCommand(<redacted>)")
    }
}
/// One authorized non-secret configuration and independently reported outcome.
#[derive(Clone, Debug, Default, Serialize)]
pub struct McpSshView {
    /// All exact SSH stdio management operations were negotiated.
    pub available: bool,
    /// Latest exact authorized observation; never an execution lease.
    pub state: Option<wire::State>,
    /// Explicit readback is required before another mutation.
    pub uncertain: bool,
    /// Safe operation result independent of general MCP observations.
    pub notice: Option<String>,
}
impl PluginsFeature {
    pub(super) async fn mcp_ssh_command(&self, command: McpSshCommand) -> Result<()> {
        let result = self.mcp_ssh_operation(command).await;
        if let Err(error) = &result {
            self.state.lock().expect("MCP SSH view").mcp_ssh.notice = Some(error.clone());
        }
        result
    }
    async fn mcp_ssh_operation(&self, command: McpSshCommand) -> Result<()> {
        let client = self
            .mcp_ssh
            .as_ref()
            .ok_or("SSH MCP administration is unavailable")?;
        let reading = matches!(command, McpSshCommand::Read { .. });
        if !reading
            && self
                .state
                .lock()
                .expect("MCP SSH admission")
                .mcp_ssh
                .uncertain
        {
            return Err("Read this server's configuration before making another change".into());
        }
        let result = match command {
            McpSshCommand::Read { target } => {
                self.state.lock().expect("MCP SSH selection").mcp_ssh.state = None;
                client.get(target).await
            }
            command => {
                let (operation, change) = match command {
                    McpSshCommand::Put { change } => (wire::Operation::Put, change),
                    McpSshCommand::Remove { change } => (wire::Operation::Remove, change),
                    McpSshCommand::Refresh { change } => (wire::Operation::Refresh, change),
                    McpSshCommand::Read { .. } => unreachable!(),
                };
                {
                    let state = self.state.lock().expect("MCP SSH revision");
                    if state.mcp_ssh.state.as_ref().is_none_or(|current| {
                        current.target != change.target || current.revision != change.expected
                    }) {
                        return Err("Read the selected server's current configuration first".into());
                    }
                }
                client.change(operation, change).await
            }
        };
        match result {
            Ok(Ok(observed)) => {
                let mut state = self.state.lock().expect("MCP SSH observation");
                state.mcp_ssh.notice = Some(if observed.apply_error.is_some() {
                    "Configuration is saved; the connection could not be applied. Read status before connecting again."
                } else if reading { "Configuration read" } else { "SSH MCP operation completed" }.into());
                state.mcp_ssh.state = Some(observed);
                state.mcp_ssh.uncertain = false;
                Ok(())
            }
            Ok(Err(failure)) => Err(match failure {
                wire::Failure::Conflict => "Configuration changed; read it again before saving",
                wire::Failure::NotFound => "This SSH MCP server was not found",
                wire::Failure::Configuration => {
                    "The configuration conflicts with the current MCP catalog"
                }
            }
            .into()),
            Err(rsi_api_protocol::ApiError::Unauthorized) => Err(
                "This action requires an exact target/server grant; connecting also requires Use"
                    .into(),
            ),
            Err(rsi_api_protocol::ApiError::Invalid(_)) => {
                Err("Invalid SSH MCP configuration or stale Host lifetime".into())
            }
            Err(rsi_api_protocol::ApiError::Capacity) => {
                Err("SSH MCP administration is at capacity".into())
            }
            Err(_) => {
                let mut state = self.state.lock().expect("MCP SSH uncertainty");
                state.mcp_ssh.state = None;
                state.mcp_ssh.uncertain = !reading;
                Err(if reading {
                    "Configuration could not be read; read it again when connected"
                } else {
                    "Operation outcome unknown. Read the configuration; do not replay the change."
                }
                .into())
            }
        }
    }
}

#[cfg(test)]
mod tests;
