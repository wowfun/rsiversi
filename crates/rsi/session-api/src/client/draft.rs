use super::{Binding, Handle, Operation, SessionError, malformed};
use rsi_agent_session_protocol::CommandRevision;
use rsi_session_protocol::{Result, SelectDraftPreset, SessionDraftView};

impl Handle {
    pub(super) async fn checked_draft_snapshot(&self) -> Result<SessionDraftView> {
        let frozen = self.frozen();
        let view: SessionDraftView = frozen.call(Operation::DraftSnapshot, &()).await?;
        if view.header != frozen.binding().header {
            return Err(malformed(Operation::DraftSnapshot));
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
        if view.header != expected_header || view.revision != revision {
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
