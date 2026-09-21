//! Native Session translation for an independently owned ACP connection.
#![deny(unsafe_code)]
#![warn(missing_docs)]

#[cfg(unix)]
mod application;
mod prompt;
mod replay;
#[cfg(unix)]
pub use application::ApplicationFactory;

/// Explicit native backend authority supplied to the stdio Application addon.
#[derive(Debug)]
pub struct AgentBackendContract;
impl rsi_meta::LocalContract for AgentBackendContract {
    const KEY: &'static str = "rsi.acp.agent-backend";
    type Service = dyn rsi_acp::server::AgentBackend;
}

use async_trait::async_trait;
use rsi_acp::{
    PeerHandle,
    server::{AgentBackend, Failure},
};
use rsi_acp_protocol::schema;
use rsi_agent_session_protocol::SessionId;
use rsi_agent_turn_protocol::TurnService;
use rsi_session_protocol::SessionHandle;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
use tokio::sync::{OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const MAX_SESSIONS: usize = 256;
const SETTLEMENT: std::time::Duration = std::time::Duration::from_secs(30);

/// Product authority preparing private MCP and an ACP-only native composition.
#[async_trait]
pub trait SessionOwner: std::fmt::Debug + Send + Sync + 'static {
    /// Prepares providers before publishing the new native draft.
    async fn create(
        &self,
        request: schema::NewSessionRequest,
    ) -> Result<Arc<dyn SessionHandle>, Failure>;
    /// Checks ownership, cwd and saved manifest, then prepares execution inputs.
    async fn restore(
        &self,
        request: schema::ResumeSessionRequest,
    ) -> Result<Arc<dyn SessionHandle>, Failure>;
    /// Returns a bounded page containing only ACP-owned native Sessions.
    async fn list(
        &self,
        request: schema::ListSessionsRequest,
    ) -> Result<schema::ListSessionsResponse, Failure>;
    /// Retires the exact private resources after controlled work has settled.
    async fn close(&self, session: &SessionId) -> Result<(), Failure>;
    /// Releases all remaining prepared inputs and stops future admission.
    async fn shutdown(&self) -> Result<(), Failure>;
}

struct Attached {
    id: SessionId,
    _resident: OwnedSemaphorePermit,
    handle: Arc<dyn SessionHandle>,
    operation: Arc<tokio::sync::Mutex<()>>,
    active: Mutex<Option<CancellationToken>>,
    settled: std::sync::atomic::AtomicBool,
    closed: std::sync::atomic::AtomicBool,
}
impl Attached {
    fn admit(&self) -> Result<OwnedMutexGuard<()>, Failure> {
        let guard = self
            .operation
            .clone()
            .try_lock_owned()
            .map_err(|_| Failure::Busy)?;
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(Failure::NotFound);
        }
        Ok(guard)
    }
    fn cancel(&self) {
        if let Some(active) = self.active.lock().expect("ACP active prompt").as_ref() {
            active.cancel();
        }
    }
}

struct SetupAdmission {
    _build: OwnedSemaphorePermit,
    resident: Option<OwnedSemaphorePermit>,
    key: Option<String>,
    pending: Arc<Mutex<BTreeSet<String>>>,
}
impl Drop for SetupAdmission {
    fn drop(&mut self) {
        if let Some(key) = &self.key {
            self.pending.lock().expect("ACP preparation").remove(key);
        }
    }
}

/// Session backend with prompt lifetime independent of protocol handler futures.
pub struct NativeAgent {
    owner: Arc<dyn SessionOwner>,
    turns: Arc<dyn TurnService>,
    sessions: Arc<Mutex<BTreeMap<String, Arc<Attached>>>>,
    builds: Arc<Semaphore>,
    residents: Arc<Semaphore>,
    pending: Arc<Mutex<BTreeSet<String>>>,
    tasks: TaskTracker,
    stopped: CancellationToken,
}
impl std::fmt::Debug for NativeAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeAgent").finish_non_exhaustive()
    }
}
impl NativeAgent {
    /// Binds explicit product preparation and Kernel observation authority.
    pub fn new(owner: Arc<dyn SessionOwner>, turns: Arc<dyn TurnService>) -> Self {
        Self {
            owner,
            turns,
            sessions: Arc::default(),
            builds: Arc::new(Semaphore::new(8)),
            residents: Arc::new(Semaphore::new(MAX_SESSIONS)),
            pending: Arc::default(),
            tasks: TaskTracker::new(),
            stopped: CancellationToken::new(),
        }
    }
    fn accepting(&self) -> Result<(), Failure> {
        if self.stopped.is_cancelled() {
            Err(Failure::Backend)
        } else {
            Ok(())
        }
    }
    fn attached(&self, id: &str) -> Result<Arc<Attached>, Failure> {
        self.accepting()?;
        self.sessions
            .lock()
            .expect("ACP attachments")
            .get(id)
            .cloned()
            .ok_or(Failure::NotFound)
    }
    fn setup(&self, key: Option<String>, existing: bool) -> Result<SetupAdmission, Failure> {
        self.accepting()?;
        let build = self
            .builds
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure::Busy)?;
        let resident = if existing {
            None
        } else {
            Some(
                self.residents
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| Failure::Busy)?,
            )
        };
        let mut pending = self.pending.lock().expect("ACP preparation");
        if let Some(key) = &key
            && !pending.insert(key.clone())
        {
            return Err(Failure::Busy);
        }
        Ok(SetupAdmission {
            _build: build,
            resident,
            key,
            pending: self.pending.clone(),
        })
    }
    async fn restore(
        &self,
        request: schema::ResumeSessionRequest,
    ) -> Result<Arc<Attached>, Failure> {
        self.accepting()?;
        // The owner validates the caller's complete setup even for a resident Session.
        let existing = self
            .sessions
            .lock()
            .expect("ACP attachments")
            .get(request.session_id.0.as_ref())
            .cloned();
        let operation = existing.as_ref().map(|entry| entry.admit()).transpose()?;
        let setup = self.setup(Some(request.session_id.0.to_string()), existing.is_some())?;
        let owner = self.owner.clone();
        self.owned_setup(
            async move { owner.restore(request).await },
            setup,
            operation,
            existing,
        )
        .await
    }

    async fn owned_setup(
        &self,
        work: impl std::future::Future<Output = Result<Arc<dyn SessionHandle>, Failure>>
        + Send
        + 'static,
        mut setup: SetupAdmission,
        operation: Option<OwnedMutexGuard<()>>,
        existing: Option<Arc<Attached>>,
    ) -> Result<Arc<Attached>, Failure> {
        let (send, receive) = tokio::sync::oneshot::channel();
        let admission = self.sessions.lock().expect("ACP attachments");
        self.accepting()?;
        let owner = self.owner.clone();
        let stopped = self.stopped.clone();
        let sessions = self.sessions.clone();
        self.tasks.spawn(async move {
            let close_abandoned=existing.is_none();
            let prepare=async {
                let handle=work.await?;
                if let Some(existing)=existing {return Ok(existing);}
                let header=handle.header().await.map_err(|_|Failure::Backend)?;
                let id=header.session_id().clone();
                let entry=Arc::new(Attached{id:id.clone(),_resident:setup.resident.take().expect("reserved attachment"),handle,operation:Arc::default(),active:Mutex::new(None),settled:std::sync::atomic::AtomicBool::new(true),closed:std::sync::atomic::AtomicBool::new(false)});
                let mut sessions=sessions.lock().expect("ACP attachments");
                if sessions.contains_key(id.as_str()) {return Err(Failure::Busy);}
                sessions.insert(id.to_string(),entry.clone());Ok(entry)
            };
            let result=tokio::select!{biased;()=stopped.cancelled()=>Err(Failure::Backend),result=prepare=>result};
            if let Err(Ok(entry))=send.send(result) && close_abandoned && owner.close(&entry.id).await.is_ok() {
                sessions.lock().expect("ACP abandoned attachment").remove(entry.id.as_str());
            }
            drop(operation);drop(setup);
        });
        drop(admission);
        receive.await.map_err(|_| Failure::Backend)?
    }
}
impl Drop for NativeAgent {
    fn drop(&mut self) {
        self.stopped.cancel();
        for session in self.sessions.lock().expect("ACP attachments").values() {
            session.cancel();
        }
    }
}

pub(crate) fn dto<T: DeserializeOwned>(value: Value) -> Result<T, Failure> {
    serde_json::from_value(value).map_err(|_| Failure::Backend)
}
pub(crate) fn encode(value: &impl Serialize) -> Result<Value, Failure> {
    serde_json::to_value(value).map_err(|_| Failure::Backend)
}

#[async_trait]
impl AgentBackend for NativeAgent {
    async fn initialize(
        &self,
        _: schema::InitializeRequest,
    ) -> Result<schema::InitializeResponse, Failure> {
        self.accepting()?;
        dto(
            json!({"protocolVersion":1,"agentInfo":{"name":"rsiversi","version":env!("CARGO_PKG_VERSION")},"agentCapabilities":{"loadSession":true,"promptCapabilities":{},"mcpCapabilities":{},"sessionCapabilities":{"list":{},"resume":{},"close":{}}},"authMethods":[]}),
        )
    }
    async fn new_session(
        &self,
        request: schema::NewSessionRequest,
    ) -> Result<schema::NewSessionResponse, Failure> {
        self.accepting()?;
        let setup = self.setup(None, false)?;
        let owner = self.owner.clone();
        let session = self
            .owned_setup(
                async move { owner.create(request).await },
                setup,
                None,
                None,
            )
            .await?;
        Ok(schema::NewSessionResponse::new(session.id.to_string()))
    }
    async fn load(
        &self,
        request: schema::LoadSessionRequest,
        peer: PeerHandle,
    ) -> Result<schema::LoadSessionResponse, Failure> {
        let session = self.restore(dto(encode(&request)?)?).await?;
        let _operation = session.admit()?;
        replay::history(&session, &peer).await?;
        peer.drain().await.map_err(|_| Failure::Backend)?;
        dto(json!({}))
    }
    async fn resume(
        &self,
        request: schema::ResumeSessionRequest,
    ) -> Result<schema::ResumeSessionResponse, Failure> {
        self.restore(request).await?;
        dto(json!({}))
    }
    async fn list(
        &self,
        request: schema::ListSessionsRequest,
    ) -> Result<schema::ListSessionsResponse, Failure> {
        self.accepting()?;
        self.owner.list(request).await
    }
    async fn prompt(
        &self,
        request: schema::PromptRequest,
        peer: PeerHandle,
    ) -> Result<schema::PromptResponse, Failure> {
        let session = self.attached(request.session_id.0.as_ref())?;
        let operation = session.admit()?;
        let content = prompt::content(request.prompt)?;
        let admission = self.sessions.lock().expect("ACP attachments");
        self.accepting()?;
        let cancellation = self.stopped.child_token();
        *session.active.lock().expect("ACP active prompt") = Some(cancellation.clone());
        let turns = self.turns.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        self.tasks.spawn(async move {
            let result = prompt::run(&session, turns.as_ref(), content, &peer, cancellation).await;
            session.active.lock().expect("ACP active prompt").take();
            drop(operation);
            let _ = send.send(result);
        });
        drop(admission);
        receive.await.map_err(|_| Failure::Backend)?
    }
    async fn cancel(&self, request: schema::CancelNotification) -> Result<(), Failure> {
        self.attached(request.session_id.0.as_ref())?.cancel();
        Ok(())
    }
    async fn close(
        &self,
        request: schema::CloseSessionRequest,
    ) -> Result<schema::CloseSessionResponse, Failure> {
        if self
            .pending
            .lock()
            .expect("ACP preparation")
            .contains(request.session_id.0.as_ref())
        {
            return Err(Failure::Busy);
        }
        let session = self.attached(request.session_id.0.as_ref())?;
        session.cancel();
        let _operation = tokio::time::timeout(SETTLEMENT, session.operation.clone().lock_owned())
            .await
            .map_err(|_| Failure::Timeout)?;
        if !session.settled.load(std::sync::atomic::Ordering::Acquire) {
            return Err(Failure::Backend);
        }
        session
            .closed
            .store(true, std::sync::atomic::Ordering::Release);
        self.owner.close(&session.id).await?;
        self.sessions
            .lock()
            .expect("ACP attachments")
            .remove(session.id.as_str());
        dto(json!({}))
    }
    async fn shutdown(&self) -> Result<(), Failure> {
        self.stopped.cancel();
        self.builds.close();
        self.residents.close();
        for session in self.sessions.lock().expect("ACP attachments").values() {
            session.cancel();
        }
        self.tasks.close();
        tokio::time::timeout(SETTLEMENT, self.tasks.wait())
            .await
            .map_err(|_| Failure::Timeout)?;
        self.sessions.lock().expect("ACP attachments").clear();
        self.owner.shutdown().await
    }
}
