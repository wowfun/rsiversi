use rsi_agent_turn_protocol::AgentCallerAuthority;
use rsi_api_protocol::{ApiError, CallOrigin, Result};
use rsi_execution::{
    ExecutionCoordinates, ExecutionLocation, ExecutionOperation, ExecutionResolver,
};

/// Trusted request authority, independent of any durable history identity or cache entry.
#[derive(Clone, Debug)]
pub enum HistoryAuthority {
    /// Actual authenticated ingress origin, checked against the requested location.
    Caller(CallOrigin),
    /// The exact live Tool caller, restricted to its immutable workspace.
    Agent(std::sync::Arc<AgentCallerAuthority>),
}
impl HistoryAuthority {
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
                    None if *coordinates.location() == ExecutionLocation::Local => {
                        resolver.admit(&CallOrigin::Local, coordinates.location())
                    }
                    None => Err(ApiError::Unauthorized),
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "authority_tests.rs"]
mod tests;
