use super::Sessions;
use rsi_agent_session_protocol::{AgentPresetId, SessionId};
use rsi_extension::{Error, Result};
use rsi_session_protocol::{BlankSession, DraftClaim, SelectDraftPreset, SessionError};
use rsi_workspace_protocol::{
    ExecutionCoordinates, ExecutionLocation, WorkspaceId, WorkspaceIngress,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    request: String,
    fresh: SessionId,
    requested: Option<SessionId>,
    workspace: Option<WorkspaceId>,
    cwd: Option<PathBuf>,
    preset: Option<AgentPresetId>,
    fork: Option<rsi_session_protocol::ForkDraft>,
}

impl Input {
    async fn coordinates(
        &self,
        ingress: &dyn WorkspaceIngress,
        origin: &rsi_api_protocol::CallOrigin,
    ) -> Result<ExecutionCoordinates> {
        match (&self.workspace, &self.cwd) {
            (Some(_), Some(_)) => Err(Error::Remote("Choose a Workspace or a directory".into())),
            (Some(id), None) => Ok(ingress
                .scoped(origin.clone())
                .get(id)
                .await
                .map_err(remote)?
                .coordinates),
            (None, Some(path)) => ingress
                .local_coordinates(origin.clone(), path)
                .await
                .map_err(remote),
            (None, None) => {
                let registry = ingress.scoped(origin.clone());
                let mut after = None;
                loop {
                    let page = registry.list(after, 64).await.map_err(remote)?;
                    if let Some(record) = page
                        .records
                        .into_iter()
                        .find(|record| record.coordinates.location() == &ExecutionLocation::Local)
                    {
                        return Ok(record.coordinates);
                    }
                    after = page.next;
                    if after.is_none() {
                        return Err(Error::Remote(
                            "Choose a directory for the new Session".into(),
                        ));
                    }
                }
            }
        }
    }
}

impl Sessions {
    pub(super) async fn create(&self, payload: Value) -> Result<Value> {
        let input: Input = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        let origin = self
            .routes
            .caller(&input.request)
            .map_err(|_| Error::Cancelled)?;
        if let Some(fork) = input.fork {
            if input.requested.is_some()
                || input.workspace.is_some()
                || input.cwd.is_some()
                || input.preset.is_some()
            {
                return Err(Error::Protocol);
            }
            let handle = self
                .ingress
                .fork_from(input.fresh, fork, origin)
                .await
                .map_err(remote)?;
            let header = handle.header().await.map_err(remote)?;
            self.routes
                .caller(&input.request)
                .map_err(|_| Error::Cancelled)?;
            return Ok(receipt(&header, false));
        }
        let coordinates = input.coordinates(self.workspaces.as_ref(), &origin).await?;
        let sessions = self.ingress.scoped(origin.clone());
        let mut id = input.fresh;
        let mut claim = DraftClaim::Exclusive;
        if let Some(requested) = input.requested {
            match sessions.attach(&requested).await {
                Ok(handle) => {
                    let header = handle.header().await.map_err(remote)?;
                    if header.fork_origin().is_some() || header.coordinates() != &coordinates {
                        return Err(Error::Remote(
                            "Session belongs to another Workspace or Agent".into(),
                        ));
                    }
                    match handle.draft_snapshot().await {
                        Ok(_)
                            if self.navigation.archived_sessions().contains(&requested)
                                || input.workspace.as_ref().is_some_and(|workspace| {
                                    !self.navigation.workspace_contains(workspace, &requested)
                                }) => {}
                        Ok(mut draft) => {
                            if let Some(preset) = input.preset.as_ref()
                                && draft.header.agent_preset_id() != preset
                            {
                                draft = handle
                                    .select_preset(SelectDraftPreset {
                                        preset_id: preset.clone(),
                                        expected_revision: draft.revision,
                                    })
                                    .await
                                    .map_err(remote)?;
                            }
                            self.routes
                                .caller(&input.request)
                                .map_err(|_| Error::Cancelled)?;
                            return Ok(receipt(&draft.header, true));
                        }
                        Err(SessionError::NotFound(_)) => {}
                        Err(error) => return Err(remote(error)),
                    }
                }
                Err(SessionError::NotFound(_)) => {
                    id = requested;
                    claim = DraftClaim::JoinIdentical;
                }
                Err(error) => return Err(remote(error)),
            }
        }
        self.routes
            .caller(&input.request)
            .map_err(|_| Error::Cancelled)?;
        let creation = BlankSession {
            coordinates,
            session_id: id,
            agent_preset_id: input.preset,
        };
        let handle = self
            .ingress
            .create_shared_blank(creation, origin.clone(), claim)
            .await
            .map_err(remote)?;
        let header = handle.header().await.map_err(remote)?;
        if let Some(workspace) = input.workspace {
            let failure = |error| {
                remote(format!(
                    "Session '{}' was created but could not join Workspace '{}': {error}",
                    header.session_id(),
                    workspace
                ))
            };
            self.navigation
                .attach_workspace_session(origin, workspace.clone(), header.session_id().clone())
                .map_err(&failure)?
                .await
                .map_err(failure)?;
        }
        self.routes
            .caller(&input.request)
            .map_err(|_| Error::Cancelled)?;
        Ok(receipt(&header, false))
    }
}

fn receipt(header: &rsi_agent_session_protocol::SessionHeader, reused: bool) -> Value {
    let blank = header
        .history_origin()
        .is_none_or(|origin| origin.effective_turns == 0);
    json!({"id":header.session_id(),"preset":header.agent_preset_id(),"reused":reused,
        "summary":{"id":header.session_id(),"cwd":header.canonical_cwd(),
            "createdAt":header.created_at_ms(),"updatedAt":header.created_at_ms(),"blank":blank,"running":false}})
}

fn remote(error: impl std::fmt::Display) -> Error {
    Error::Remote(error.to_string().chars().take(1000).collect())
}
