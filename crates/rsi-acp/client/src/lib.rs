//! External ACP Session ownership over supplied, bounded protocol authority.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod configuration;
mod incoming;
mod operations;
use rsi_acp::{Incoming, Peer, PeerHandle};
use rsi_acp_journal::{ConversationId, Journal, Snapshot, Status};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex as AsyncMutex, watch},
    task::JoinHandle,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const CONTROL: Duration = Duration::from_secs(30);

/// Categorical failures with no command, credential or raw peer error text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    /// Invalid caller input or malformed peer data.
    #[error("invalid external conversation input")]
    Input,
    /// A prompt or setup operation is already active.
    #[error("external conversation is busy")]
    Busy,
    /// Operation was not explicitly advertised by this peer.
    #[error("external agent does not support this operation")]
    Unsupported,
    /// Exact permission identity or connection generation no longer exists.
    #[error("external interaction is stale")]
    Stale,
    /// Peer explicitly returned a JSON-RPC error, without remote text disclosure.
    #[error("external agent rejected the operation")]
    Remote,
    /// Delivery or settlement is unknown. The prompt must not be resent.
    #[error("external operation outcome is unknown")]
    Unknown,
    /// Local observation could not be safely stored or read.
    #[error("external observation storage failed")]
    Journal,
}
/// External operation result.
pub type Result<T> = std::result::Result<T, Error>;

pub use rsi_acp_protocol::service::{Permission, PermissionOption, Setup};
struct PendingPermission {
    incoming: Incoming,
    view: Permission,
}

struct State {
    journal: Journal,
    snapshot: Mutex<Snapshot>,
    id: ConversationId,
    generation: u64,
    target: Mutex<Option<String>>,
    port: PeerHandle,
    stop: CancellationToken,
    closing: AtomicBool,
    initialized: AtomicBool,
    operation: Arc<AsyncMutex<()>>,
    active: Mutex<Option<CancellationToken>>,
    permissions: Mutex<BTreeMap<String, PendingPermission>>,
    processed: watch::Sender<u64>,
    changed: watch::Sender<u64>,
    tasks: TaskTracker,
}
impl State {
    fn changed(&self) {
        self.changed
            .send_modify(|revision| *revision = revision.saturating_add(1));
    }
    fn snapshot(&self) -> Snapshot {
        self.snapshot.lock().expect("ACP client snapshot").clone()
    }
    async fn status(&self, status: Status) -> Result<Snapshot> {
        let snapshot = self
            .journal
            .settle(&self.id, self.generation, status)
            .await
            .map_err(|_| Error::Journal)?;
        *self.snapshot.lock().expect("ACP client snapshot") = snapshot.clone();
        self.changed();
        Ok(snapshot)
    }
    fn accepting(&self) -> Result<()> {
        if self.closing.load(Ordering::Acquire) || self.stop.is_cancelled() || self.port.is_closed()
        {
            Err(Error::Unknown)
        } else {
            Ok(())
        }
    }
    fn target(&self) -> Result<String> {
        self.target
            .lock()
            .expect("ACP target")
            .clone()
            .ok_or(Error::Input)
    }
    fn bind_target(&self, target: &str) -> Result<()> {
        if target.is_empty() || target.len() > 256 {
            return Err(Error::Input);
        }
        let mut current = self.target.lock().expect("ACP target");
        if current.as_ref().is_some_and(|current| current != target) {
            return Err(Error::Input);
        }
        *current = Some(target.into());
        Ok(())
    }
    async fn barrier(&self, through: u64) -> Result<()> {
        let mut processed = self.processed.subscribe();
        tokio::time::timeout(CONTROL, async {
            loop {
                if *processed.borrow_and_update() >= through {
                    return Ok(());
                }
                tokio::select! { biased;
                    () = self.stop.cancelled() => return Err(Error::Unknown),
                    result = processed.changed() => result.map_err(|_| Error::Unknown)?,
                }
            }
        })
        .await
        .map_err(|_| Error::Unknown)?
    }
}

/// Host-retained owner. Dropping a view handle does not close this peer.
pub struct Client {
    state: Arc<State>,
    reader: Option<JoinHandle<Result<()>>>,
}
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalClient").finish_non_exhaustive()
    }
}
impl Client {
    /// Attaches an already admitted peer to a reserved journal generation.
    /// Routing is installed immediately, before any setup request can emit updates.
    pub fn attach(peer: Peer, journal: Journal, snapshot: Snapshot) -> Result<Self> {
        snapshot.validate().map_err(|_| Error::Input)?;
        if snapshot.generation == 0 {
            return Err(Error::Input);
        }
        let state = Arc::new(State {
            id: snapshot.id.clone(),
            generation: snapshot.generation,
            target: Mutex::new(snapshot.remote.clone()),
            snapshot: Mutex::new(snapshot),
            journal,
            port: peer.handle(),
            stop: CancellationToken::new(),
            closing: AtomicBool::new(false),
            initialized: AtomicBool::new(false),
            operation: Arc::new(AsyncMutex::new(())),
            active: Mutex::new(None),
            permissions: Mutex::new(BTreeMap::new()),
            processed: watch::channel(0).0,
            changed: watch::channel(0).0,
            tasks: TaskTracker::new(),
        });
        let reader = tokio::spawn(incoming::run(peer, state.clone()));
        Ok(Self {
            state,
            reader: Some(reader),
        })
    }
    /// Returns a detachable controller with no ownership of peer lifetime.
    pub fn handle(&self) -> Handle {
        Handle(self.state.clone())
    }
    /// Negotiates and explicitly creates/resumes/loads the selected remote Session.
    pub async fn initialize(
        &self,
        mode: Setup,
        mcp_servers: Vec<rsi_acp_protocol::schema::McpServer>,
        selections: &[rsi_acp_protocol::configuration::ConfigSelection],
    ) -> Result<Snapshot> {
        operations::initialize(&self.state, mode, mcp_servers, selections).await
    }
    /// Cancels active work, optionally closes the remote Session, and joins peer cleanup.
    ///
    /// # Panics
    /// Panics if a prior internal panic poisoned the owner state.
    pub async fn close(mut self) -> Result<Snapshot> {
        {
            let _admission = self.state.permissions.lock().expect("ACP permissions");
            self.state.closing.store(true, Ordering::Release);
        }
        let settled = operations::cancel(&self.state).await.is_ok();
        let before = self.state.snapshot();
        let known = settled
            && !matches!(
                before.status,
                Status::Unknown | Status::Starting | Status::Loading | Status::Running
            );
        let mut closed = known;
        if known && before.capabilities.close {
            closed = if let Ok(target) = self.state.target() {
                operations::request(
                    &self.state,
                    "session/close",
                    &serde_json::json!({"sessionId": target}),
                    CONTROL,
                )
                .await
                .is_ok()
            } else {
                false
            };
        }
        self.state.stop.cancel();
        self.state.tasks.close();
        tokio::time::timeout(CONTROL, self.state.tasks.wait())
            .await
            .map_err(|_| Error::Unknown)?;
        if let Some(reader) = self.reader.take() {
            reader.await.map_err(|_| Error::Unknown)??;
        }
        self.state
            .status(if closed {
                Status::Closed
            } else {
                Status::Unknown
            })
            .await
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.state.closing.store(true, Ordering::Release);
        self.state.stop.cancel();
    }
}

/// Detachable view/controller; the Host must retain the separate `Client` owner.
#[derive(Clone)]
pub struct Handle(Arc<State>);
impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalHandle").finish_non_exhaustive()
    }
}
impl Handle {
    /// Whether this exact peer is initialized and currently accepts control.
    pub fn connected(&self) -> bool {
        self.0.accepting().is_ok()
    }
    /// Reads the latest observed state, without creating or resuming a peer.
    pub fn snapshot(&self) -> Snapshot {
        self.0.snapshot()
    }
    /// First categorical transport failure, without raw peer payload or stderr.
    pub fn failure(&self) -> Option<rsi_acp::Error> {
        self.0.port.failure()
    }
    /// Observes bounded revision signals; full history remains paged in the journal.
    pub fn observe(&self) -> watch::Receiver<u64> {
        self.0.changed.subscribe()
    }
    /// Returns current bounded permission metadata for this exact connection.
    ///
    /// # Panics
    /// Panics if a prior internal panic poisoned the permission state.
    pub fn permissions(&self) -> Vec<Permission> {
        self.0
            .permissions
            .lock()
            .expect("ACP permissions")
            .values()
            .map(|pending| pending.view.clone())
            .collect()
    }
    /// Admits one prompt whose task survives this caller's future being dropped.
    pub async fn submit(
        &self,
        prompt: Vec<rsi_acp_protocol::schema::ContentBlock>,
    ) -> Result<Snapshot> {
        operations::submit(self.0.clone(), prompt).await
    }
    /// Sends cancellation and waits for the actual prompt response, without retry.
    pub async fn cancel(&self) -> Result<Snapshot> {
        self.0.accepting()?;
        operations::cancel(&self.0).await?;
        Ok(self.snapshot())
    }
    /// Answers only the exact live peer option; no local always-grant is created.
    pub async fn answer(&self, generation: u64, permission: &str, option: &str) -> Result<()> {
        incoming::answer(&self.0, generation, permission, option).await
    }
}
