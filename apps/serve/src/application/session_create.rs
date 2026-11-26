use super::{BoundSessions, Sessions};
use rsi_agent_session_protocol::{AgentPresetId, SessionId};
use rsi_extension::{Error, Result};
use rsi_session_protocol::{BlankSession, DraftClaim, SessionError};
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
            return Ok(receipt(&header, None));
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
                    if !self.navigation.archived_sessions().contains(&requested)
                        && input.workspace.as_ref().is_none_or(|workspace| {
                            self.navigation.workspace_contains(workspace, &requested)
                        })
                    {
                        let preset = match &input.preset {
                            Some(id) => id.clone(),
                            None => self.context
                                .lookup_local::<rsi_agent_composition_protocol::AgentCompositionContract>()
                                .ok_or_else(|| Error::Remote("The Host default preset is unavailable".into()))?
                                .default_preset_id().await.map_err(remote)?,
                        };
                        match handle.select_agent_preset(preset.clone()).await {
                            Ok(_) => {
                                self.routes
                                    .caller(&input.request)
                                    .map_err(|_| Error::Cancelled)?;
                                return Ok(receipt(&header, Some(&preset)));
                            }
                            Err(SessionError::NotFound(_)) => {}
                            Err(error) => return Err(remote(error)),
                        }
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
        let mut result = receipt(&header, None);
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
            result["workspace_updated_at"] =
                json!(self.navigation.workspace_changed_at(&workspace));
        }
        self.routes
            .caller(&input.request)
            .map_err(|_| Error::Cancelled)?;
        Ok(result)
    }
}

impl BoundSessions {
    pub(super) async fn create_plugin_child(&self, payload: Value) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Tool {
            generation: String,
            effect: rsi_agent_session_protocol::EffectId,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Child {
            request: String,
            parent: SessionId,
            session_id: SessionId,
            task_name: String,
            plugin: String,
            preset: Option<AgentPresetId>,
            tool: Option<Tool>,
            cwd: Option<PathBuf>,
        }
        let child: Child = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        if child.request != self.plugin_callback.id() {
            return Err(Error::Remote(
                "Plugin child creation requires the application callback".into(),
            ));
        }
        use rsi_agent_turn_protocol::PluginChildSource;
        let source = if let Some(tool) = child.tool {
            let caller = self
                .tools
                .as_ref()
                .and_then(|tools| tools.started_tool(&tool.generation, &child.parent, &tool.effect))
                .ok_or_else(|| Error::Remote("Application tool invocation has ended".into()))?;
            PluginChildSource::Tool(caller)
        } else {
            let parent = self
                .interactive_header(&child.request, &child.parent)
                .await?;
            if parent.coordinates().location() != &ExecutionLocation::Local {
                return Err(Error::Remote(
                    "Application plugins require a local workspace".into(),
                ));
            }
            PluginChildSource::Session(Box::new(parent))
        };
        let directory = match child.cwd {
            Some(path) => {
                let origin = self
                    .routes
                    .caller(&child.request)
                    .map_err(|_| Error::Cancelled)?;
                Some(
                    self.workspaces
                        .local_coordinates(origin, &path)
                        .await
                        .map_err(remote)?,
                )
            }
            None => None,
        };
        let turns = self
            .context
            .lookup_local::<rsi_agent_turn_protocol::TurnServiceContract>()
            .ok_or_else(|| Error::Remote("Agent runtime unavailable".into()))?;
        let header = turns
            .create_plugin_child(rsi_agent_turn_protocol::CreatePluginChild {
                source,
                directory,
                session_id: child.session_id,
                task_name: child.task_name,
                plugin: child.plugin,
                preset: child.preset,
            })
            .await
            .map_err(remote)?;
        Ok(receipt(&header, None))
    }
}

fn receipt(
    header: &rsi_agent_session_protocol::SessionHeader,
    reused: Option<&AgentPresetId>,
) -> Value {
    json!({"id":header.session_id(),"preset":reused.unwrap_or_else(|| header.agent_preset_id()),"reused":reused.is_some(),
        "summary":super::session_page::catalog_item(header, header.created_at_ms(), 0, false)})
}

fn remote(error: impl std::fmt::Display) -> Error {
    Error::Remote(error.to_string().chars().take(1000).collect())
}
