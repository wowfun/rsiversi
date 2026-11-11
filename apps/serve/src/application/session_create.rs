use super::Sessions;
use rsi_agent_session_protocol::{AgentPresetId, SessionId};
use rsi_extension::{Error, Result};
use rsi_session_protocol::{CreateSession, SelectDraftPreset, SessionError};
use rsi_workspace_protocol::{ExecutionLocation, WorkspaceId, WorkspaceRegistry};
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
    async fn workspace(&self, registry: &dyn WorkspaceRegistry) -> Result<WorkspaceId> {
        match (&self.workspace, &self.cwd) {
            (Some(_), Some(_)) => Err(Error::Remote("Choose a Workspace or a directory".into())),
            (Some(id), None) => Ok(registry.get(id).await.map_err(remote)?.id),
            (None, Some(path)) => Ok(registry.get_or_create(path).await.map_err(remote)?.id),
            (None, None) => {
                let mut after = None;
                loop {
                    let page = registry.list(after, 64).await.map_err(remote)?;
                    if let Some(record) = page
                        .records
                        .into_iter()
                        .find(|record| record.coordinates.location() == &ExecutionLocation::Local)
                    {
                        return Ok(record.id);
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
        let sessions = self.ingress.scoped(origin.clone());
        let workspace = if let Some(fork) = &input.fork {
            if input.requested.is_some()
                || input.workspace.is_some()
                || input.cwd.is_some()
                || input.preset.is_some()
            {
                return Err(Error::Protocol);
            }
            let source = sessions
                .read_summary(&fork.source_session_id)
                .await
                .map_err(remote)?;
            self.workspaces
                .scoped(origin.clone())
                .find(&rsi_workspace_protocol::WorkspaceKey::from_coordinates(
                    source.header.coordinates(),
                ))
                .await
                .map_err(remote)?
                .ok_or_else(|| Error::Remote("Workspace is not registered".into()))?
                .id
        } else {
            input
                .workspace(self.workspaces.scoped(origin.clone()).as_ref())
                .await?
        };
        let mut id = input.fresh;
        if let Some(requested) = input.requested {
            match sessions.attach(&requested).await {
                Ok(handle) => {
                    let header = handle.header().await.map_err(remote)?;
                    let selected = self
                        .workspaces
                        .scoped(origin.clone())
                        .get(&workspace)
                        .await
                        .map_err(remote)?;
                    if header.fork_origin().is_some()
                        || header.coordinates() != &selected.coordinates
                    {
                        return Err(Error::Remote(
                            "Session belongs to another Workspace or Agent".into(),
                        ));
                    }
                    match handle.draft_snapshot().await {
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
                            return Ok(receipt(&draft.header));
                        }
                        Err(SessionError::NotFound(_)) => {}
                        Err(error) => return Err(remote(error)),
                    }
                }
                Err(SessionError::NotFound(_)) => id = requested,
                Err(error) => return Err(remote(error)),
            }
        }
        self.routes
            .caller(&input.request)
            .map_err(|_| Error::Cancelled)?;
        let handle = sessions
            .create(CreateSession {
                workspace_id: workspace,
                session_id: id,
                agent_preset_id: input.preset,
                fork: input.fork,
            })
            .await
            .map_err(remote)?;
        let header = handle.header().await.map_err(remote)?;
        self.routes
            .caller(&input.request)
            .map_err(|_| Error::Cancelled)?;
        Ok(receipt(&header))
    }
}

fn receipt(header: &rsi_agent_session_protocol::SessionHeader) -> Value {
    let blank = header
        .history_origin()
        .is_none_or(|origin| origin.effective_turns == 0);
    json!({"id":header.session_id(),"preset":header.agent_preset_id(),
        "summary":{"id":header.session_id(),"cwd":header.canonical_cwd(),
            "createdAt":header.created_at_ms(),"updatedAt":header.created_at_ms(),"blank":blank,"running":false}})
}

fn remote(error: impl std::fmt::Display) -> Error {
    Error::Remote(error.to_string().chars().take(1000).collect())
}
