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

pub(crate) trait BackendServices {
    fn required<C: rsi_meta::LocalContract>(&self) -> crate::Result<Arc<C::Service>>;
}
impl BackendServices for crate::RunningRsi {
    fn required<C: rsi_meta::LocalContract>(&self) -> crate::Result<Arc<C::Service>> {
        crate::required_local::<C>(&self.host, C::KEY)
    }
}
impl BackendServices for rsi_meta::Context {
    fn required<C: rsi_meta::LocalContract>(&self) -> crate::Result<Arc<C::Service>> {
        self.lookup_local::<C>()
            .ok_or_else(|| crate::RsiError::Boot(format!("{} is unavailable", C::KEY)))
    }
}
pub(crate) fn backend(source: &impl BackendServices) -> crate::Result<Arc<dyn AgentBackend>> {
    let owner = Arc::new(Owner {
        sessions: source.required::<rsi_session_protocol::SessionContract>()?,
        drafts: source.required::<rsi_session_protocol::SessionDraftControlContract>()?,
        workspaces: source.required::<rsi_workspace_protocol::WorkspaceRegistryContract>()?,
        store: source.required::<rsi_agent_store_protocol::SessionStoreContract>()?,
        inputs: source.required::<InputsContract>()?,
        models: source.required::<rsi_ai_protocol::LanguageModelsContract>()?,
        attached: Mutex::new(BTreeSet::new()),
        attachment: Arc::new(()),
    });
    Ok(Arc::new(rsi_acp_agent::NativeAgent::new(
        owner,
        source.required::<rsi_agent_turn_protocol::TurnServiceContract>()?,
    )))
}
#[derive(Debug)]
struct Owner {
    sessions: Arc<dyn SessionService>,
    drafts: Arc<dyn SessionDraftControl>,
    workspaces: Arc<dyn rsi_workspace_protocol::WorkspaceRegistry>,
    store: Arc<dyn SessionStore>,
    inputs: Arc<PrivateInputs>,
    models: Arc<dyn rsi_ai_protocol::LanguageModels>,
    attached: Mutex<BTreeSet<SessionId>>,
    attachment: Arc<()>,
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
        self.inputs.claim(&id, &self.attachment)?;
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
                if self.inputs.close(&id).await.is_ok() {
                    self.inputs.release_claim(&id, &self.attachment);
                    self.attached
                        .lock()
                        .expect("ACP owned Sessions")
                        .remove(&id);
                }
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
        let seed = match self
            .store
            .read_controls(
                &id,
                0,
                1,
                rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
            )
            .await
        {
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
        self.inputs.claim(&id, &self.attachment)?;
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
            if newly_attached && self.inputs.close(&id).await.is_ok() {
                self.inputs.release_claim(&id, &self.attachment);
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
        let next = page
            .next
            .as_ref()
            .map(|cursor| {
                serde_json::to_vec(cursor)
                    .map(hex::encode)
                    .map_err(|_| Failure::Backend)
            })
            .transpose()?;
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
    async fn config_options(
        &self,
        session: &dyn SessionHandle,
    ) -> Result<Vec<schema::SessionConfigOption>, Failure> {
        crate::acp_configuration::options(session, self.models.as_ref()).await
    }
    async fn set_config_option(
        &self,
        session: &dyn SessionHandle,
        request: schema::SetSessionConfigOptionRequest,
    ) -> Result<Vec<schema::SessionConfigOption>, Failure> {
        crate::acp_configuration::set(session, self.models.as_ref(), request).await
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
        self.inputs.release_claim(session, &self.attachment);
        self.attached
            .lock()
            .expect("ACP owned Sessions")
            .remove(session);
        Ok(())
    }
    async fn shutdown(&self) -> Result<(), Failure> {
        let sessions = self.attached.lock().expect("ACP owned Sessions").clone();
        close_all(
            sessions
                .iter()
                .map(|session| self.close(session))
                .collect::<Vec<_>>(),
        )
        .await
    }
}

async fn close_all(
    operations: impl IntoIterator<Item = impl std::future::Future<Output = Result<(), Failure>>>,
) -> Result<(), Failure> {
    let mut failure = None;
    for operation in operations {
        if let Err(error) = operation.await {
            failure.get_or_insert(error);
        }
    }
    failure.map_or(Ok(()), Err)
}
#[cfg(test)]
mod shutdown_tests {
    use super::*;
    #[tokio::test]
    async fn failure_does_not_skip_later_session_cleanup() {
        let completed = Mutex::new(Vec::new());
        let result = close_all((0..3).map(|id| {
            let completed = &completed;
            async move {
                completed.lock().unwrap().push(id);
                if id == 0 {
                    Err(Failure::Busy)
                } else if id == 2 {
                    Err(Failure::Backend)
                } else {
                    Ok(())
                }
            }
        }))
        .await;
        assert!(matches!(result, Err(Failure::Busy)));
        assert_eq!(*completed.lock().unwrap(), [0, 1, 2]);
    }
}
