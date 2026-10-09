use rsi_agent_turn_protocol::AgentCallerAuthority;
use rsi_api_protocol::{ApiError, CallOrigin, Result};
use rsi_execution::{ExecutionCoordinates, ExecutionOperation, ExecutionResolver};

/// Trusted request authority, independent of any durable history identity or cache entry.
#[derive(Clone, Debug)]
pub enum HistoryAuthority {
    /// Actual authenticated ingress origin, checked against the requested location.
    Caller(CallOrigin),
    /// The exact live Tool caller, restricted to its immutable workspace.
    Agent(std::sync::Arc<AgentCallerAuthority>),
}
impl HistoryAuthority {
    pub(super) fn principal(&self) -> String {
        use sha2::{Digest as _, Sha256};
        let identity = match self {
            Self::Caller(CallOrigin::Local) => "local".to_owned(),
            Self::Caller(CallOrigin::Device(device)) => format!("device:{}", device.id.as_str()),
            Self::Agent(caller) => format!("agent:{}", caller.session_id()),
        };
        hex::encode(Sha256::digest(identity.as_bytes()))
    }
    pub(super) fn narrow(&self, scope: &rsi_history_api::QueryScope) -> Result<()> {
        if let Self::Agent(caller) = self {
            let workspace = rsi_workspace_protocol::WorkspaceId::from_coordinates(
                caller.header().coordinates(),
            );
            let allowed = match scope {
                rsi_history_api::QueryScope::Workspace {
                    workspace: requested,
                } => *requested == workspace,
                rsi_history_api::QueryScope::Conversation { source } => {
                    source.workspace == workspace
                }
                rsi_history_api::QueryScope::AccessibleHost => false,
            };
            if !allowed {
                return Err(ApiError::Unauthorized);
            }
        }
        if let Self::Caller(CallOrigin::Device(device)) = self
            && device.revoked.is_cancelled()
        {
            return Err(ApiError::Unauthorized);
        }
        Ok(())
    }
    pub(super) fn admit(
        &self,
        resolver: &dyn ExecutionResolver,
        coordinates: &ExecutionCoordinates,
    ) -> Result<ExecutionOperation> {
        match self {
            Self::Caller(origin) => resolver.admit(origin, coordinates.location()),
            Self::Agent(caller) => {
                if caller.header().coordinates() != coordinates {
                    return Err(ApiError::Unauthorized);
                }
                match caller.execution() {
                    Some(execution) => execution.admit().map_err(|error| match error {
                        rsi_process::ProcessError::Capacity => ApiError::Capacity,
                        _ => ApiError::Unauthorized,
                    }),
                    None => Err(ApiError::Unauthorized),
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "authority_tests.rs"]
mod tests;
