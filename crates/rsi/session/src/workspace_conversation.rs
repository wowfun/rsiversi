use super::{
    AgentPresetId, Arc, DraftClaim, LocalSessionService, Result, SessionError, SessionHandle,
    drafts, map_store_error, map_workspace_error,
};
use rsi_session_protocol::{BlankSession, OpenWorkspaceConversation};

impl LocalSessionService {
    pub(super) async fn open_workspace_conversation(
        &self,
        request: OpenWorkspaceConversation,
    ) -> Result<Arc<dyn SessionHandle>> {
        let workspace = self
            .workspace
            .get(&request.workspace_id)
            .await
            .map_err(|e| map_workspace_error(&e))?;
        let _admission = self.admit(workspace.coordinates.location())?;
        let explicit = request.agent_preset_id.is_some();
        let preset = match request.agent_preset_id {
            Some(preset) => preset,
            None => self
                .composition
                .default_preset_id()
                .await
                .map_err(|e| SessionError::Backend(e.to_string()))?,
        };
        if let Some(navigation) = self.navigation() {
            let archived = navigation.archived_sessions();
            let mut after = None;
            loop {
                let page = self
                    .store
                    .list_session_activity(
                        &rsi_execution::ExecutionLocations::all(),
                        rsi_agent_store_protocol::ActivityScope::WorkspaceWithoutFacts(
                            &workspace.coordinates,
                        ),
                        after.as_ref(),
                        64,
                    )
                    .await
                    .map_err(map_store_error)?;
                after = page.sessions.last().map(|row| row.cursor());
                for row in page.sessions {
                    if !archived.contains(&row.session_id)
                        && navigation.workspace_contains(&workspace.id, &row.session_id)
                        && let Some(handle) = self
                            .workspace_candidate(&row.session_id, &preset, explicit)
                            .await?
                    {
                        return Ok(handle);
                    }
                }
                if !page.has_more {
                    break;
                }
            }
        }
        Ok(self
            .open_draft(
                drafts::DraftRequest::Shared(BlankSession {
                    workspace: Some(workspace.id),
                    coordinates: workspace.coordinates,
                    session_id: request.fresh,
                    agent_preset_id: Some(preset),
                }),
                DraftClaim::Exclusive,
            )
            .await?)
    }

    async fn workspace_candidate(
        &self,
        id: &super::SessionId,
        preset: &AgentPresetId,
        explicit: bool,
    ) -> Result<Option<Arc<dyn SessionHandle>>> {
        let handle = match self.attach_local(id).await {
            Ok(handle) => handle,
            Err(
                SessionError::NotFound(_)
                | SessionError::Api(rsi_api_protocol::ApiError::Unauthorized),
            ) => return Ok(None),
            Err(error) => return Err(error),
        };
        {
            let state = handle.state.lock().await;
            let header = state.header()?;
            if header.history_origin().is_some()
                || header.execution_owner().is_some()
                || header.protection().is_some()
            {
                return Ok(None);
            }
        }
        if !explicit && handle.credential_is_host_owner()? {
            match handle.select_agent_preset(preset.clone()).await {
                Ok(_) => {}
                Err(SessionError::NotFound(_) | SessionError::CommandRevisionConflict { .. }) => {
                    return Ok(None);
                }
                Err(error) => return Err(error),
            }
        } else {
            let view = handle.preset_view().await?;
            if view.revision.is_none() || view.preset_id != *preset {
                return Ok(None);
            }
        }
        Ok(Some(handle))
    }
}
