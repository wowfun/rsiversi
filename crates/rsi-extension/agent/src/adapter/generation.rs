use super::{
    AgentAdapter, CancellationToken, ContributionError, ContributionResult, ToolError, async_trait,
    invalid,
};
use rsi_agent_composition_protocol::GenerationValidator;
use rsi_agent_session_protocol::{DomainStateView, EXTENSION_JOURNAL_DOMAIN, SessionHeader};

#[async_trait]
impl GenerationValidator for AgentAdapter {
    async fn validate(
        &self,
        header: &SessionHeader,
        domains: &[DomainStateView],
        cancellation: CancellationToken,
    ) -> ContributionResult<()> {
        let update = domains
            .iter()
            .find(|update| update.snapshot.identity().id() == EXTENSION_JOURNAL_DOMAIN)
            .ok_or_else(|| invalid("candidate journal is absent"))?;
        let journal = self.handle.decode(&update.snapshot).map_err(invalid)?;
        self.load_snapshot(header, None, update.revision, &journal, true, cancellation)
            .await
            .map_err(|error| {
                validation_error(error, self.connection.disconnect_token().is_cancelled())
            })
    }
}

fn validation_error(error: ToolError, disconnected: bool) -> ContributionError {
    if disconnected {
        return ContributionError::Closed;
    }
    match error {
        ToolError::Cancelled => ContributionError::Closed,
        ToolError::Capacity => ContributionError::Capacity,
        other => invalid(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn candidate_load_distinguishes_retryable_transport_and_invalid_state() {
        assert!(matches!(
            validation_error(ToolError::Capacity, false),
            ContributionError::Capacity
        ));
        assert!(matches!(
            validation_error(ToolError::Cancelled, false),
            ContributionError::Closed
        ));
        assert!(matches!(
            validation_error(ToolError::Execution("wire closed".into()), true),
            ContributionError::Closed
        ));
        assert!(matches!(
            validation_error(ToolError::Execution("unsupported checkpoint".into()), false),
            ContributionError::Invalid(_)
        ));
    }
}
