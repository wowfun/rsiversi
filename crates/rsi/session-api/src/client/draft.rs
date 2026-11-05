use super::{Binding, Handle, Operation, SessionError, malformed};
use rsi_agent_session_protocol::CommandRevision;
use rsi_session_protocol::{Result, SelectDraftPreset, SessionDraftView};

impl Handle {
    pub(super) async fn checked_draft_snapshot(&self) -> Result<SessionDraftView> {
        let frozen = self.frozen();
        let view: SessionDraftView = frozen.call(Operation::DraftSnapshot, &()).await?;
        self.adopt_draft_view(&frozen, view, Operation::DraftSnapshot)
    }

    pub(super) async fn discard_checked_inputs(
        &self,
        request: rsi_session_protocol::DiscardDraftInputs,
    ) -> Result<SessionDraftView> {
        request.validate()?;
        let frozen = self.frozen();
        let view: SessionDraftView = frozen.call(Operation::DiscardDraftInputs, &request).await?;
        if view
            .command_inputs
            .iter()
            .any(|input| request.message_ids.contains(&input.message.message_id))
        {
            return Err(malformed(Operation::DiscardDraftInputs));
        }
        self.adopt_draft_view(&frozen, view, Operation::DiscardDraftInputs)
    }

    fn adopt_draft_view(
        &self,
        frozen: &Self,
        view: SessionDraftView,
        operation: Operation,
    ) -> Result<SessionDraftView> {
        if view.validate().is_err() || view.header != frozen.binding().header {
            return Err(malformed(operation));
        }
        let mut binding = self.binding.write().expect("Session binding poisoned");
        if binding.target == frozen.target() {
            binding.draft_revision = Some(binding.draft_revision.unwrap_or(0).max(view.revision));
        }
        Ok(view)
    }

    pub(super) async fn select_checked_preset(
        &self,
        request: SelectDraftPreset,
    ) -> Result<SessionDraftView> {
        let revision = request
            .expected_revision
            .checked_add(1)
            .ok_or_else(|| SessionError::Invalid("draft revision cannot advance".into()))?;
        let frozen = self.frozen();
        let expected_header = frozen
            .binding()
            .header
            .with_agent_preset_id(request.preset_id.clone())
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        let result: Result<SessionDraftView> = frozen.call(Operation::SelectPreset, &request).await;
        if matches!(&result, Err(SessionError::CommandRevisionConflict { expected, .. }) if *expected != (CommandRevision::Draft { revision: request.expected_revision }))
        {
            return Err(malformed(Operation::SelectPreset));
        }
        let view = result?;
        if view.validate().is_err()
            || !view.command_inputs.is_empty()
            || view.header != expected_header
            || view.revision != revision
        {
            return Err(malformed(Operation::SelectPreset));
        }
        let mut binding = self.binding.write().expect("Session binding poisoned");
        if binding.target != frozen.target()
            || binding
                .draft_revision
                .is_some_and(|current| current > revision)
        {
            return Err(malformed(Operation::SelectPreset));
        }
        let mut target = binding.target.clone();
        target.header_key = view
            .header
            .fingerprint()
            .map_err(|_| malformed(Operation::SelectPreset))?;
        *binding = Binding {
            header: view.header.clone(),
            target,
            draft_revision: Some(revision),
        };
        Ok(view)
    }
}
