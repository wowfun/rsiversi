use crate::acp_inputs::{InputsContract, PRESET, PrivateInputs};
use async_trait::async_trait;
use rsi_acp::server::{AgentBackend, Failure};
use rsi_acp_protocol::schema;
use rsi_agent_composition_protocol::AgentGenerationSeed;
use rsi_agent_session_protocol::{
    AgentControlRecordBody, AgentPresetId, DomainMutationSource, SessionId,
};
use rsi_agent_store_protocol::SessionStore;
use rsi_session_protocol::{
    CreateSession, DraftRelease, RecentSessionCursor, SessionDraftControl, SessionHandle,
    SessionService,
};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

pub(crate) fn backend(running: &crate::RunningRsi) -> crate::Result<Arc<dyn AgentBackend>> {
    let owner = Arc::new(Owner {
        sessions: running.session_service()?,
        drafts: crate::required_local::<rsi_session_protocol::SessionDraftControlContract>(
            &running.host,
            "draft lifetime",
        )?,
        workspaces: running.workspace_registry()?,
        store: crate::required_local::<rsi_agent_store_protocol::SessionStoreContract>(
            &running.host,
            "Agent Store",
        )?,
        inputs: crate::required_local::<InputsContract>(&running.host, "ACP private inputs")?,
        attached: Mutex::new(BTreeSet::new()),
    });
    Ok(Arc::new(rsi_acp_agent::NativeAgent::new(
        owner,
        crate::required_local::<rsi_agent_turn_protocol::TurnServiceContract>(
            &running.host,
            "Agent turns",
        )?,
    )))
}
#[derive(Debug)]
struct Owner {
    sessions: Arc<dyn SessionService>,
    drafts: Arc<dyn SessionDraftControl>,
    workspaces: Arc<dyn rsi_workspace_protocol::WorkspaceRegistry>,
    store: Arc<dyn SessionStore>,
    inputs: Arc<PrivateInputs>,
    attached: Mutex<BTreeSet<SessionId>>,
}
#[async_trait]
impl rsi_acp_agent::SessionOwner for Owner {
    async fn create(
        &self,
        request: schema::NewSessionRequest,
    ) -> Result<Arc<dyn SessionHandle>, Failure> {
        let cwd = rsi_session::canonical_workspace_directory(&request.cwd)
            .await
            .map_err(|_| Failure::Parameters)?;
        let cwd_text = cwd.to_str().ok_or(Failure::Parameters)?.to_owned();
        let mut entropy = [0_u8; 16];
        getrandom::fill(&mut entropy).map_err(|_| Failure::Backend)?;
        let id = SessionId::new(format!("acp-{}", hex::encode(entropy)))
            .map_err(|_| Failure::Backend)?;
        self.attached
            .lock()
            .expect("ACP owned Sessions")
            .insert(id.clone());
        let result = async {
            self.inputs
                .prepare(id.clone(), cwd_text, request.mcp_servers, None)
                .await?;
            let workspace = self
                .workspaces
                .get_or_create(&cwd)
                .await
                .map_err(|_| Failure::Backend)?;
            self.sessions
                .create(CreateSession {
                    workspace_id: workspace.id,
                    session_id: id.clone(),
                    agent_preset_id: Some(
                        AgentPresetId::new(PRESET).expect("private preset identity"),
                    ),
                })
                .await
                .map_err(|_| Failure::Backend)
        }
        .await;
        match result {
            Ok(handle) => {
                self.attached.lock().expect("ACP owned Sessions").insert(id);
                Ok(handle)
            }
            Err(error) => {
                let _cleanup = self.inputs.close(&id).await;
                self.attached
                    .lock()
                    .expect("ACP owned Sessions")
                    .remove(&id);
                Err(error)
            }
        }
    }
    async fn restore(
        &self,
        request: schema::ResumeSessionRequest,
    ) -> Result<Arc<dyn SessionHandle>, Failure> {
        let id = SessionId::new(request.session_id.0.as_ref()).map_err(|_| Failure::Parameters)?;
        let handle = self
            .sessions
            .attach(&id)
            .await
            .map_err(|_| Failure::NotFound)?;
        let header = handle.header().await.map_err(|_| Failure::Backend)?;
        if header.agent_preset_id().as_str() != PRESET || header.fork_origin().is_some() {
            return Err(Failure::NotFound);
        }
        let cwd = rsi_session::canonical_workspace_directory(&request.cwd)
            .await
            .map_err(|_| Failure::Parameters)?;
        if cwd.to_str() != Some(header.canonical_cwd()) {
            return Err(Failure::Parameters);
        }
        let seed = match self.store.read_controls(&id, 0, 1).await {
            Ok(page) => {
                let Some(record) = page.records.first() else {
                    return Err(Failure::Backend);
                };
                let AgentControlRecordBody::DomainStateCommitted { commit } = record.body() else {
                    return Err(Failure::Backend);
                };
                if !matches!(commit.source(), DomainMutationSource::Baseline) {
                    return Err(Failure::Backend);
                }
                Some(
                    AgentGenerationSeed::new(
                        commit
                            .updates()
                            .iter()
                            .map(|update| update.snapshot().clone())
                            .collect(),
                    )
                    .map_err(|_| Failure::Backend)?,
                )
            }
            Err(rsi_agent_store_protocol::StoreError::NotFound(_))
                if self
                    .attached
                    .lock()
                    .expect("ACP owned Sessions")
                    .contains(&id) =>
            {
                None
            }
            Err(_) => return Err(Failure::Backend),
        };
        let newly_attached = self
            .attached
            .lock()
            .expect("ACP owned Sessions")
            .insert(id.clone());
        let result = self
            .inputs
            .prepare(
                id.clone(),
                header.canonical_cwd().to_owned(),
                request.mcp_servers,
                seed,
            )
            .await;
        if let Err(error) = result {
            if newly_attached {
                let _cleanup = self.inputs.close(&id).await;
                self.attached
                    .lock()
                    .expect("ACP owned Sessions")
                    .remove(&id);
            }
            return Err(error);
        }
        Ok(handle)
    }
    async fn list(
        &self,
        request: schema::ListSessionsRequest,
    ) -> Result<schema::ListSessionsResponse, Failure> {
        let cursor: Option<RecentSessionCursor> = request
            .cursor
            .as_ref()
            .map(|cursor| {
                if cursor.len() > 2048 {
                    return Err(Failure::Parameters);
                }
                serde_json::from_slice(&hex::decode(cursor).map_err(|_| Failure::Parameters)?)
                    .map_err(|_| Failure::Parameters)
            })
            .transpose()?;
        let page = self
            .sessions
            .list_recent(cursor.as_ref(), 64)
            .await
            .map_err(|_| Failure::Backend)?;
        let next = if page.has_more {
            page.sessions
                .last()
                .map(|last| {
                    serde_json::to_vec(&last.cursor())
                        .map(hex::encode)
                        .map_err(|_| Failure::Backend)
                })
                .transpose()?
        } else {
            None
        };
        let sessions = page
            .sessions
            .into_iter()
            .filter(|entry| {
                entry.header.agent_preset_id().as_str() == PRESET
                    && entry.header.fork_origin().is_none()
                    && request
                        .cwd
                        .as_ref()
                        .is_none_or(|cwd| cwd.to_str() == Some(entry.header.canonical_cwd()))
            })
            .map(|entry| {
                schema::SessionInfo::new(
                    entry.header.session_id().to_string(),
                    entry.header.canonical_cwd(),
                )
            })
            .collect();
        Ok(schema::ListSessionsResponse::new(sessions).next_cursor(next))
    }
    async fn close(&self, session: &SessionId) -> Result<(), Failure> {
        if !self
            .attached
            .lock()
            .expect("ACP owned Sessions")
            .contains(session)
        {
            return Err(Failure::NotFound);
        }
        if self.drafts.release_draft(session) == DraftRelease::Busy {
            return Err(Failure::Busy);
        }
        self.inputs.close(session).await?;
        self.attached
            .lock()
            .expect("ACP owned Sessions")
            .remove(session);
        Ok(())
    }
    async fn shutdown(&self) -> Result<(), Failure> {
        let sessions = std::mem::take(&mut *self.attached.lock().expect("ACP owned Sessions"));
        for session in sessions {
            if self.drafts.release_draft(&session) == DraftRelease::Busy {
                return Err(Failure::Busy);
            }
            self.inputs.close(&session).await?;
        }
        Ok(())
    }
}
