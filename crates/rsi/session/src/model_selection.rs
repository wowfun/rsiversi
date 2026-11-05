//! Finite selection reads; execution captures again at its Step boundary.
use super::{
    HandleState, LocalSessionHandle, Result, SessionError, SessionHeader, map_ai_error,
    map_store_error,
};
use rsi_agent_session_protocol::ModelSelection;

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
        let resolve = |snapshot: Option<&rsi_agent_session_protocol::DomainSnapshot>| {
            rsi_agent_model_selection::resolve_selection(header, snapshot, None)
                .map_err(|error| SessionError::Invalid(error.to_string()))
        };
        {
            let state = self.state.lock().await;
            match &*state {
                HandleState::Fresh(draft) => {
                    return resolve(
                        draft
                            .baseline()
                            .initial_state(rsi_agent_model_selection::DOMAIN),
                    );
                }
                HandleState::Attached(_) => {}
                HandleState::Expired => return Err(SessionError::NotFound("draft lease".into())),
            }
        }
        let page = self
            .store
            .read_domain_states(
                self.session_id(),
                None,
                Some(rsi_agent_model_selection::DOMAIN),
            )
            .await
            .map_err(map_store_error)?;
        resolve(page.states.first().map(|state| &state.snapshot))
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
