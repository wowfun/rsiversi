use super::{PluginsFeature, Result};
use rsi_configuration_api::ssh as wire;
use serde::{Deserialize, Serialize};
/// Bounded ephemeral commands; identity paths never enter the retained view.
#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SshCommand {
    /// Explicitly refresh targets and reconcile any uncertain prior mutation.
    Read,
    /// Create or replace one candidate with exact revision CAS.
    Put {
        /// Complete bounded candidate request.
        request: wire::PutCandidate,
    },
    /// Local-only host-key and identity confirmation.
    Trust {
        /// Exact ephemeral Local trust input.
        request: wire::ConfirmTrust,
    },
    /// Explicitly initialize a fresh target connection.
    Connect {
        /// Displayed target revision and connection epoch.
        request: wire::ConnectionRequest,
    },
    /// Explicitly close the selected connection.
    Disconnect {
        /// Displayed target revision and connection epoch.
        request: wire::ConnectionRequest,
    },
    /// Resolve a directory only through the selected target lease.
    Resolve {
        /// Displayed connection and target path.
        request: wire::ResolveDirectory,
    },
}
impl std::fmt::Debug for SshCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SshCommand(<redacted>)")
    }
}
/// Non-secret target projection independent of profile configuration values.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SshView {
    /// Required target operations were negotiated.
    pub available: bool,
    /// Local-only trust confirmation was negotiated.
    pub can_trust: bool,
    /// One complete caller-visible target catalog.
    pub catalog: Option<wire::Catalog>,
    /// One successful directory resolution, never a permission token.
    pub directory: Option<rsi_execution_protocol::ExecutionCoordinates>,
    /// Further mutations require an explicit observation after uncertainty.
    pub uncertain: bool,
    /// Safe status without arbitrary server text or identity path.
    pub notice: Option<String>,
}
impl PluginsFeature {
    pub(super) async fn ssh_command(&self, command: SshCommand) -> Result<()> {
        let result = self.ssh_operation(command).await;
        if let Err(error) = &result {
            self.state.lock().expect("SSH workbench").ssh.notice = Some(error.clone());
        }
        result
    }
    async fn ssh_operation(&self, command: SshCommand) -> Result<()> {
        let client = self
            .ssh
            .as_ref()
            .ok_or("SSH target management is unavailable")?;
        if matches!(command, SshCommand::Read) {
            let Ok(Ok(catalog)) = client.catalog().await else {
                self.state.lock().expect("SSH read failure").ssh.catalog = None;
                return Err(
                    "SSH targets unavailable; refresh again when the connection is ready".into(),
                );
            };
            let mut state = self.state.lock().expect("SSH refresh");
            state.ssh.catalog = Some(catalog);
            state.ssh.directory = None;
            state.ssh.uncertain = false;
            state.ssh.notice = None;
            return Ok(());
        }
        if self
            .state
            .lock()
            .expect("SSH uncertain operation")
            .ssh
            .uncertain
        {
            return Err("Refresh SSH targets before another operation".into());
        }
        if let SshCommand::Resolve { request } = command {
            let coordinates = match client.resolve_directory(request).await {
                Ok(Ok(value)) => value,
                Ok(Err(error)) => return Err(failure(error).into()),
                Err(_) => {
                    return Err(
                        "Target directory could not be resolved; refresh its connection".into(),
                    );
                }
            };
            self.state.lock().expect("SSH directory").ssh.directory = Some(coordinates);
            return Ok(());
        }
        let reply = match command {
            SshCommand::Put { request } => client.put_candidate(request).await,
            SshCommand::Trust { request } => client.confirm_trust(request).await,
            SshCommand::Connect { request } => client.connect(request).await,
            SshCommand::Disconnect { request } => client.disconnect(request).await,
            SshCommand::Read | SshCommand::Resolve { .. } => {
                unreachable!("handled read operations")
            }
        };
        match reply {
            Ok(Ok(target)) => {
                let mut state = self.state.lock().expect("SSH mutation result");
                if let Some(catalog) = &mut state.ssh.catalog {
                    catalog
                        .targets
                        .retain(|item| item.candidate.target != target.candidate.target);
                    catalog.targets.push(target);
                    catalog
                        .targets
                        .sort_by(|a, b| a.candidate.target.cmp(&b.candidate.target));
                }
                state.ssh.directory = None;
                state.ssh.notice = Some("SSH target state updated".into());
                Ok(())
            }
            Ok(Err(error)) => Err(failure(error).into()),
            Err(rsi_api_protocol::ApiError::Unauthorized) => {
                Err("This operation requires its exact SSH grant or Local trust authority".into())
            }
            Err(rsi_api_protocol::ApiError::Invalid(_)) => {
                Err("Invalid SSH target input or stale Host lifetime; refresh targets".into())
            }
            Err(rsi_api_protocol::ApiError::Capacity) => {
                Err("SSH target capacity is currently full".into())
            }
            Err(_) => {
                self.state
                    .lock()
                    .expect("SSH unknown mutation")
                    .ssh
                    .uncertain = true;
                Err("SSH operation outcome unknown. Refresh targets; do not repeat the mutation automatically.".into())
            }
        }
    }
}
fn failure(value: wire::Failure) -> &'static str {
    match value {
        wire::Failure::Busy {} => {
            "Another SSH target operation is in progress; retry when it finishes"
        }
        wire::Failure::Conflict {} => "Target revision or connection changed; refresh SSH targets",
        wire::Failure::TrustRequired {} => {
            "A Local operator must confirm this target's host key and identity"
        }
        wire::Failure::IdentityUnavailable {} => {
            "The confirmed identity is unavailable or changed; Local confirmation is required"
        }
        wire::Failure::HelperUnavailable {} => {
            "The installed distribution has no matching verified SSH helper"
        }
        wire::Failure::ConnectionFailed {} => {
            "SSH initialization failed; check target trust and runtime prerequisites"
        }
    }
}

#[cfg(test)]
mod tests;
