//! Finite selection reads; execution captures again at its Step boundary.
use super::{
    HandleState, LocalSessionHandle, Result, SessionError, SessionHeader, map_ai_error,
    map_store_error,
};
use rsi_agent_session_protocol::{DomainRevision, DomainStateView, ModelSelection};

impl LocalSessionHandle {
    pub(super) async fn current_model_read(
        &self,
        header: &SessionHeader,
    ) -> Result<rsi_session_protocol::ModelSelectionRead> {
        use rsi_session_protocol::{ModelAvailability, ModelSelectionRead};
        let selection = self.current_model_selection(header).await?;
        let availability = match self.language.describe(&selection.model) {
            Ok(description) => match description
                .profile()
                .reasoning_efforts()
                .resolve(selection.reasoning_effort.as_ref())
            {
                Ok(_) => ModelAvailability::Available { description },
                Err(error) => ModelAvailability::Unavailable {
                    reason: error.to_string().chars().take(512).collect(),
                },
            },
            Err(error) => ModelAvailability::Unavailable {
                reason: error.to_string().chars().take(512).collect(),
            },
        };
        Ok(ModelSelectionRead {
            selection,
            availability,
        })
    }
    pub(super) async fn current_model_selection(
        &self,
        header: &SessionHeader,
    ) -> Result<ModelSelection> {
        let initial = {
            let state = self.state.lock().await;
            match &*state {
                HandleState::Fresh(draft) => Some(
                    draft
                        .baseline()
                        .initial_states()
                        .into_iter()
                        .map(|snapshot| DomainStateView {
                            revision: DomainRevision::new(0),
                            snapshot,
                        })
                        .collect::<Vec<_>>(),
                ),
                HandleState::Attached(_) => None,
                HandleState::Expired => return Err(SessionError::NotFound("draft lease".into())),
            }
        };
        let domains = if let Some(initial) = initial {
            initial
        } else {
            self.store
                .read_domain_states(self.session_id(), None)
                .await
                .map_err(map_store_error)?
                .states
                .into_iter()
                .map(|state| DomainStateView {
                    revision: state.head.revision,
                    snapshot: state.snapshot,
                })
                .collect()
        };
        rsi_agent_model_selection::resolve_selection(header, &domains, None)
            .map_err(|error| SessionError::Invalid(error.to_string()))
    }
    pub(super) fn validate_model_selection(&self, selection: &ModelSelection) -> Result<()> {
        selection
            .validate()
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        self.language
            .describe(&selection.model)
            .map_err(|error| map_ai_error(&error))?
            .profile()
            .reasoning_efforts()
            .resolve(selection.reasoning_effort.as_ref())
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        Ok(())
    }
}
