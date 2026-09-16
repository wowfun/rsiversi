//! Finite resource I/O shares generation selection and bounded capture admission.

use super::*;
use rsi_agent_composition_protocol::SessionResourceAdapter;
use rsi_agent_session_protocol::{ValidatedResourceRequest, ValidatedResourceResponse};
use rsi_agent_turn_protocol::SessionResources;

#[async_trait]
impl SessionResources for AgentKernel {
    async fn read_resource(
        &self,
        session: &SessionId,
        request: ValidatedResourceRequest,
        cancellation: CancellationToken,
    ) -> TurnResult<ValidatedResourceResponse> {
        let _admission = self
            .inner
            .projection_admission
            .clone()
            .try_acquire_owned()
            .map_err(|_| TurnError::ProjectionCapacity)?;
        let stop = self.inner.submission_admission.closed.child_token();
        let _guard = stop.clone().drop_guard();
        let operation = async {
            let (header, pin) = self.projection_generation(session).await?;
            SessionResourceAdapter::new(pin)
                .read(
                    header,
                    request,
                    &rsi_meta::Execution::native(tokio::runtime::Handle::current()),
                    stop.clone(),
                )
                .await
                .map_err(|error| match error {
                    rsi_agent_composition_protocol::ContributionError::Capacity => {
                        TurnError::ProjectionCapacity
                    }
                    rsi_agent_composition_protocol::ContributionError::Closed => {
                        TurnError::ShuttingDown
                    }
                    error => TurnError::Invalid(error.to_string()),
                })
        };
        cancellation
            .run_until_cancelled(stop.run_until_cancelled(operation))
            .await
            .ok_or(TurnError::Cancelled)?
            .ok_or(TurnError::ShuttingDown)?
    }
}
