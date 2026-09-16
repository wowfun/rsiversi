use super::{HandleState, LocalSessionHandle, Result, SessionError, map_turn_error};
use rsi_agent_composition_protocol::SessionResourceAdapter;
use rsi_agent_session_protocol::SessionResourceRequest;
use rsi_session_protocol::ResourceSnapshot;
use std::{sync::Arc, time::Duration};

impl LocalSessionHandle {
    pub(super) async fn resource_read(
        &self,
        request: SessionResourceRequest,
    ) -> Result<ResourceSnapshot> {
        let request = request
            .validated()
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        let reservation = self.resource_retention.reserve()?;
        let stop = self.projection_stopped.child_token();
        let _guard = stop.clone().drop_guard();
        let deadline = self.execution.deadline_after(Duration::from_secs(30));
        stop.run_until_cancelled(deadline.timeout(async {
            let _activity = self.begin_activity()?;
            self.reconcile_fresh_read().await?;
            let captured = {
                let state = self.state.lock().await;
                match &*state {
                    HandleState::Fresh(draft) => Some((
                        Arc::new(draft.header().clone()),
                        draft.composition().clone(),
                    )),
                    HandleState::Attached(_) => None,
                    HandleState::Expired => {
                        return Err(SessionError::NotFound("draft lease".into()));
                    }
                }
            };
            let response = if let Some((header, pin)) = captured {
                SessionResourceAdapter::new(pin)
                    .read(header, request, &self.execution, stop.clone())
                    .await
                    .map_err(|error| match error {
                        rsi_agent_composition_protocol::ContributionError::Capacity => {
                            SessionError::Capacity
                        }
                        rsi_agent_composition_protocol::ContributionError::Closed => {
                            SessionError::ShuttingDown
                        }
                        error => SessionError::Invalid(error.to_string()),
                    })?
            } else {
                self.resources
                    .as_ref()
                    .ok_or_else(|| SessionError::NotFound("Session resource owner".into()))?
                    .read_resource(self.session_id(), request, stop.clone())
                    .await
                    .map_err(map_turn_error)?
            };
            reservation.retain_validated(response)
        }))
        .await
        .ok_or(SessionError::ShuttingDown)?
        .map_err(|_| SessionError::Backend("Session resource deadline elapsed".into()))?
    }
}
