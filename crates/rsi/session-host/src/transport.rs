use crate::owner::{
    HostEpoch, SESSION_HOST_PROTOCOL_EPOCH, SessionHostError, SessionHostPaths,
    session_host_product_build, validate_launch_key,
};
use async_trait::async_trait;
use futures_util::StreamExt as _;
use rsi_agent_session_protocol::{
    AgentPresetId, MAXIMUM_SESSION_FACT_BYTES, SessionFact, SessionHeader, SessionId, TurnId,
    validate_turn_text,
};
use rsi_agent_store_protocol::{MAXIMUM_STORE_FACT_PAGE_BYTES, StoreBackwardFactPage};
use rsi_agent_turn_protocol::{CancelResult, TurnError, TurnObservation, TurnUpdate};
use rsi_ai_protocol::{ImageRequest, ModelRef};
use rsi_approval_protocol::{ApprovalDecision, ApprovalRequest};
use rsi_sandbox::SandboxMode;
use rsi_session::{
    CreateSession, RecentSessionCursor, RecentSessionPage, SessionApplication,
    SessionApplicationError, SessionHandle, SessionHistoryPage, SessionSummary, SubmitDirectImage,
    SubmitText, TurnReceipt, canonical_workspace_directory,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

const MAXIMUM_FRAME_BYTES: usize = MAXIMUM_SESSION_FACT_BYTES + 64 * 1024;
const MAXIMUM_HANDSHAKE_FRAME_BYTES: usize = 16 * 1024;
const MAXIMUM_IN_FLIGHT_FRAME_BYTES: usize = 64 * 1024 * 1024;
const MAXIMUM_CONNECTIONS: usize = 128;
const MAXIMUM_UNPUBLISHED_DRAFTS: usize = 1024;
const MAXIMUM_SEQUENCE_ITEMS: usize = 1024;
const UNPUBLISHED_DRAFT_IDLE_TIMEOUT: Duration = Duration::from_hours(1);
const UNPUBLISHED_DRAFT_SWEEP_INTERVAL: Duration = Duration::from_mins(1);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const HANDSHAKE_READ_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(10);
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(50);
const RESPONSE_READ_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum time graceful server shutdown waits for admitted connections.
pub const SESSION_HOST_DRAIN_TIMEOUT: Duration = Duration::from_mins(1);

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ClientFrame {
    Hello {
        protocol_epoch: u32,
        product_build: String,
        launch_key: String,
        host_epoch: HostEpoch,
    },
    Request {
        request_id: String,
        operation: WireOperation,
    },
}

#[derive(Debug)]
struct UnpublishedDraft {
    handle: Arc<dyn SessionHandle>,
    expires_at: tokio::time::Instant,
    _admission: OwnedSemaphorePermit,
}

type UnpublishedDrafts = tokio::sync::Mutex<BTreeMap<SessionId, UnpublishedDraft>>;

#[derive(Clone, Debug)]
struct FrameReadBudget {
    bytes: Arc<Semaphore>,
}

impl FrameReadBudget {
    fn new(maximum_bytes: usize) -> Self {
        Self {
            bytes: Arc::new(Semaphore::new(maximum_bytes)),
        }
    }

    async fn acquire(&self, bytes: usize) -> Result<OwnedSemaphorePermit, SessionHostError> {
        let bytes = u32::try_from(bytes)
            .map_err(|_| SessionHostError::Invalid("frame length exceeds admission".into()))?;
        Arc::clone(&self.bytes)
            .acquire_many_owned(bytes)
            .await
            .map_err(|_| SessionHostError::Io("Session Host frame admission closed".into()))
    }
}

impl Default for FrameReadBudget {
    fn default() -> Self {
        Self::new(MAXIMUM_IN_FLIGHT_FRAME_BYTES)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum WireOperation {
    Create {
        cwd: String,
        session_id: Option<SessionId>,
        agent_preset_id: Option<AgentPresetId>,
    },
    Attach {
        session_id: SessionId,
    },
    ListRecent {
        after: Option<WireRecentCursor>,
        limit: usize,
    },
    Header {
        session_id: SessionId,
    },
    SubmitText {
        session_id: SessionId,
        turn_id: TurnId,
        text: String,
        model: Option<ModelRef>,
        sandbox: Option<SandboxMode>,
    },
    SubmitImage {
        session_id: SessionId,
        turn_id: TurnId,
        model: ModelRef,
        request: ImageRequest,
    },
    Cancel {
        session_id: SessionId,
        turn_id: TurnId,
        reason: Option<String>,
    },
    History {
        session_id: SessionId,
        exclusive_before_seq: Option<u64>,
        limit: usize,
    },
    Subscribe {
        session_id: SessionId,
        after_seq: u64,
    },
    PendingApprovals {
        session_id: SessionId,
    },
    AnswerApproval {
        session_id: SessionId,
        approval_id: String,
        decision: ApprovalDecision,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WireRecentCursor {
    created_at_ms: u64,
    session_id: SessionId,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ServerFrame {
    HelloOk {
        protocol_epoch: u32,
        product_build: String,
        launch_key: String,
        host_epoch: HostEpoch,
    },
    HelloRejected {
        reason: String,
    },
    Response {
        request_id: String,
        response: Option<WireResponse>,
        error: Option<WireError>,
    },
    Event {
        request_id: String,
        session_id: SessionId,
        update: WireUpdate,
    },
    Item {
        request_id: String,
        item: WireItem,
    },
    End {
        request_id: String,
        error: Option<WireError>,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum WireResponse {
    Session {
        header: SessionHeader,
    },
    RecentStart {
        has_more: bool,
    },
    Receipt {
        session_id: SessionId,
        turn_id: TurnId,
        accepted_seq: u64,
    },
    Cancel {
        accepted: bool,
        already_terminal: bool,
    },
    HistoryStart {
        before_seq: u64,
        durable_seq: u64,
        has_more: bool,
    },
    PendingApprovalsStart,
    ApprovalAnswer {
        accepted: bool,
    },
    Subscribed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum WireItem {
    Session {
        header: SessionHeader,
    },
    Fact {
        session_id: SessionId,
        fact: SessionFact,
    },
    Approval {
        request: ApprovalRequest,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum WireUpdate {
    Fact {
        fact: Box<SessionFact>,
        durable_seq: u64,
    },
    Durable {
        durable_seq: u64,
    },
}

impl From<TurnUpdate> for WireUpdate {
    fn from(update: TurnUpdate) -> Self {
        match update {
            TurnUpdate::Fact { fact, durable_seq } => Self::Fact {
                fact: Box::new((*fact).clone()),
                durable_seq,
            },
            TurnUpdate::Durable { durable_seq } => Self::Durable { durable_seq },
        }
    }
}

impl From<WireUpdate> for TurnUpdate {
    fn from(update: WireUpdate) -> Self {
        match update {
            WireUpdate::Fact { fact, durable_seq } => Self::Fact {
                fact: Arc::new(*fact),
                durable_seq,
            },
            WireUpdate::Durable { durable_seq } => Self::Durable { durable_seq },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WireError {
    Invalid { message: String },
    NotFound { value: String },
    Conflict { session: String, turn: String },
    Capacity,
    ShuttingDown,
    Backend { message: String },
}

impl From<SessionApplicationError> for WireError {
    fn from(error: SessionApplicationError) -> Self {
        match error {
            SessionApplicationError::Invalid(message) => Self::Invalid { message },
            SessionApplicationError::NotFound(value) => Self::NotFound { value },
            SessionApplicationError::Conflict { session, turn } => Self::Conflict { session, turn },
            SessionApplicationError::Capacity => Self::Capacity,
            SessionApplicationError::ShuttingDown => Self::ShuttingDown,
            SessionApplicationError::Backend(message) => Self::Backend { message },
        }
    }
}

impl From<WireError> for SessionApplicationError {
    fn from(error: WireError) -> Self {
        match error {
            WireError::Invalid { message } => Self::Invalid(message),
            WireError::NotFound { value } => Self::NotFound(value),
            WireError::Conflict { session, turn } => Self::Conflict { session, turn },
            WireError::Capacity => Self::Capacity,
            WireError::ShuttingDown => Self::ShuttingDown,
            WireError::Backend { message } => Self::Backend(message),
        }
    }
}

#[derive(Debug)]
struct PublishedSocket {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl PublishedSocket {
    fn bind(paths: &SessionHostPaths) -> Result<(UnixListener, Self), SessionHostError> {
        create_private_runtime_directory(paths.runtime_directory())?;
        remove_stale_socket_after_failed_probe(paths.socket())?;
        // Keep this name shorter than `host.sock`: a public path at the Unix
        // sockaddr limit must remain publishable. The owner lease serializes
        // publishers; any crash-left socket is still probed before removal.
        let staging = paths.runtime_directory().join(".s");
        remove_stale_socket_after_failed_probe(&staging)?;
        let listener = UnixListener::bind(&staging).map_err(io_error)?;
        let staged_metadata = fs::symlink_metadata(&staging).map_err(io_error)?;
        if !staged_metadata.file_type().is_socket() {
            let _ = fs::remove_file(&staging);
            return Err(SessionHostError::Invalid(
                "staged Session Host endpoint is not a socket".into(),
            ));
        }
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o600)).map_err(io_error)?;
        if let Err(error) = fs::hard_link(&staging, paths.socket()) {
            let _ = fs::remove_file(&staging);
            return Err(io_error(error));
        }
        let published = fs::symlink_metadata(paths.socket()).map_err(io_error)?;
        let _ = fs::remove_file(&staging);
        if !published.file_type().is_socket()
            || published.dev() != staged_metadata.dev()
            || published.ino() != staged_metadata.ino()
        {
            return Err(SessionHostError::Invalid(
                "published Session Host endpoint changed during staged bind".into(),
            ));
        }
        Ok((
            listener,
            Self {
                path: paths.socket().to_owned(),
                device: published.dev(),
                inode: published.ino(),
            },
        ))
    }
}

impl Drop for PublishedSocket {
    fn drop(&mut self) {
        let Some(parent) = self.path.parent() else {
            return;
        };
        let tombstone = parent.join(format!(".host.cleanup.{}.sock", self.inode));
        if fs::rename(&self.path, &tombstone).is_err() {
            return;
        }
        let matches = fs::symlink_metadata(&tombstone).is_ok_and(|metadata| {
            metadata.file_type().is_socket()
                && metadata.dev() == self.device
                && metadata.ino() == self.inode
        });
        if matches {
            let _ = fs::remove_file(tombstone);
        } else if !self.path.exists() {
            let _ = fs::rename(tombstone, &self.path);
        }
    }
}

/// Same-user bounded, closed-shape framed-JSON server for one Host generation.
#[derive(Debug)]
pub struct UdsSessionServer {
    listener: UnixListener,
    published: PublishedSocket,
    application: Arc<dyn SessionApplication>,
    unpublished_drafts: Arc<UnpublishedDrafts>,
    draft_admission: Arc<Semaphore>,
    frame_budget: FrameReadBudget,
    launch_key: String,
    host_epoch: HostEpoch,
    expected_uid: u32,
}

impl UdsSessionServer {
    /// Stages and atomically publishes one private daemon endpoint.
    pub fn bind(
        paths: &SessionHostPaths,
        application: Arc<dyn SessionApplication>,
        launch_key: impl Into<String>,
        host_epoch: HostEpoch,
    ) -> Result<Self, SessionHostError> {
        let _product_build = session_host_product_build()?;
        paths.validate_daemon_endpoint()?;
        let launch_key = launch_key.into();
        validate_launch_key(&launch_key)?;
        let (listener, published) = PublishedSocket::bind(paths)?;
        let expected_uid = rustix::process::geteuid().as_raw();
        Ok(Self {
            listener,
            published,
            application,
            unpublished_drafts: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
            draft_admission: Arc::new(Semaphore::new(MAXIMUM_UNPUBLISHED_DRAFTS)),
            frame_budget: FrameReadBudget::default(),
            launch_key,
            host_epoch,
            expected_uid,
        })
    }

    /// Accepts bounded same-user clients until cancellation, then drains for at most 60 seconds.
    pub async fn serve(self, cancellation: CancellationToken) -> Result<(), SessionHostError> {
        let permits = Arc::new(Semaphore::new(MAXIMUM_CONNECTIONS));
        let mut tasks = JoinSet::new();
        let mut draft_sweep = tokio::time::interval_at(
            tokio::time::Instant::now() + UNPUBLISHED_DRAFT_SWEEP_INTERVAL,
            UNPUBLISHED_DRAFT_SWEEP_INTERVAL,
        );
        draft_sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let accepted = tokio::select! {
                biased;
                () = cancellation.cancelled() => break,
                _ = draft_sweep.tick() => {
                    prune_expired_drafts(&self.unpublished_drafts).await;
                    continue;
                }
                _ = tasks.join_next(), if !tasks.is_empty() => continue,
                accepted = self.listener.accept() => accepted,
            };
            let Ok((stream, _)) = accepted else {
                // Transient accept failures (ECONNABORTED, EMFILE/ENFILE
                // under churn, EINTR) must not kill a long-lived daemon.
                tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
                continue;
            };
            let Ok(credentials) = stream.peer_cred() else {
                continue;
            };
            if credentials.uid() != self.expected_uid {
                continue;
            }
            let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                continue;
            };
            let application = Arc::clone(&self.application);
            let drafts = Arc::clone(&self.unpublished_drafts);
            let draft_admission = Arc::clone(&self.draft_admission);
            let frame_budget = self.frame_budget.clone();
            let launch_key = self.launch_key.clone();
            let host_epoch = self.host_epoch.clone();
            let shutdown = cancellation.clone();
            tasks.spawn(async move {
                let _permit = permit;
                let _ = handle_connection(
                    stream,
                    ConnectionContext {
                        application,
                        drafts,
                        draft_admission,
                        frame_budget,
                        launch_key,
                        host_epoch,
                        shutdown,
                    },
                )
                .await;
            });
        }
        drop(self.listener);
        let drain_deadline = tokio::time::Instant::now() + SESSION_HOST_DRAIN_TIMEOUT;
        while !tasks.is_empty() {
            match tokio::time::timeout_at(drain_deadline, tasks.join_next()).await {
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => {
                    tasks.abort_all();
                    while tasks.join_next().await.is_some() {}
                    break;
                }
            }
        }
        drop(self.published);
        Ok(())
    }
}

struct ConnectionContext {
    application: Arc<dyn SessionApplication>,
    drafts: Arc<UnpublishedDrafts>,
    draft_admission: Arc<Semaphore>,
    frame_budget: FrameReadBudget,
    launch_key: String,
    host_epoch: HostEpoch,
    shutdown: CancellationToken,
}

async fn handle_connection(
    mut stream: UnixStream,
    context: ConnectionContext,
) -> Result<(), SessionHostError> {
    if !negotiate_handshake(&mut stream, &context).await? {
        return Ok(());
    }
    let request: ClientFrame = read_frame_with_timeout(
        &mut stream,
        MAXIMUM_FRAME_BYTES,
        &context.frame_budget,
        REQUEST_READ_TIMEOUT,
        "client request",
    )
    .await?;
    let ClientFrame::Request {
        request_id,
        operation,
    } = request
    else {
        return Err(SessionHostError::Invalid(
            "exactly one request must follow hello".into(),
        ));
    };
    validate_request_id(&request_id)?;
    if let WireOperation::Subscribe {
        session_id,
        after_seq,
    } = operation
    {
        return serve_subscription(
            &mut stream,
            &request_id,
            &session_id,
            after_seq,
            context.application,
            context.drafts,
            context.shutdown,
        )
        .await;
    }
    if matches!(
        &operation,
        WireOperation::ListRecent { .. }
            | WireOperation::History { .. }
            | WireOperation::PendingApprovals { .. }
    ) {
        return serve_sequence(
            &mut stream,
            &request_id,
            context.application,
            context.drafts,
            operation,
        )
        .await;
    }
    let result = execute_operation(
        context.application,
        context.drafts,
        context.draft_admission,
        operation,
    )
    .await;
    let (response, error) = match result {
        Ok(response) => (Some(response), None),
        Err(error) => (None, Some(error.into())),
    };
    write_frame(
        &mut stream,
        &ServerFrame::Response {
            request_id,
            response,
            error,
        },
    )
    .await
}

async fn negotiate_handshake(
    stream: &mut UnixStream,
    context: &ConnectionContext,
) -> Result<bool, SessionHostError> {
    let expected_product_build = session_host_product_build()?;
    let hello: ClientFrame = read_frame_with_timeout(
        stream,
        MAXIMUM_HANDSHAKE_FRAME_BYTES,
        &context.frame_budget,
        HANDSHAKE_READ_TIMEOUT,
        "client handshake",
    )
    .await?;
    match hello {
        ClientFrame::Hello {
            protocol_epoch,
            product_build,
            launch_key: requested_key,
            host_epoch: requested_epoch,
        } if protocol_epoch == SESSION_HOST_PROTOCOL_EPOCH
            && product_build == expected_product_build
            && requested_key == context.launch_key
            && requested_epoch == context.host_epoch =>
        {
            write_frame(
                stream,
                &ServerFrame::HelloOk {
                    protocol_epoch: SESSION_HOST_PROTOCOL_EPOCH,
                    product_build: expected_product_build.into(),
                    launch_key: context.launch_key.clone(),
                    host_epoch: context.host_epoch.clone(),
                },
            )
            .await?;
        }
        ClientFrame::Hello { .. } => {
            write_frame(
                stream,
                &ServerFrame::HelloRejected {
                    reason:
                        "protocol epoch, product build, launch key, or Host epoch is incompatible"
                            .into(),
                },
            )
            .await?;
            return Ok(false);
        }
        ClientFrame::Request { .. } => {
            return Err(SessionHostError::Invalid(
                "the first client frame must be hello".into(),
            ));
        }
    }
    Ok(true)
}

async fn execute_operation(
    application: Arc<dyn SessionApplication>,
    drafts: Arc<UnpublishedDrafts>,
    draft_admission: Arc<Semaphore>,
    operation: WireOperation,
) -> rsi_session::Result<WireResponse> {
    if let WireOperation::SubmitText { text, .. } = &operation {
        validate_turn_text(text)
            .map_err(|error| SessionApplicationError::Invalid(error.to_string()))?;
    }
    match operation {
        WireOperation::Create {
            cwd,
            session_id,
            agent_preset_id,
        } => {
            create_draft(
                application,
                drafts,
                draft_admission,
                cwd,
                session_id,
                agent_preset_id,
            )
            .await
        }
        WireOperation::Attach { session_id } | WireOperation::Header { session_id } => {
            let handle = get_handle(&application, &drafts, &session_id).await?;
            Ok(WireResponse::Session {
                header: handle.header().await?,
            })
        }
        WireOperation::SubmitText {
            session_id,
            turn_id,
            text,
            model,
            sandbox,
        } => {
            let handle = get_handle(&application, &drafts, &session_id).await?;
            let result = handle
                .submit_text(SubmitText {
                    turn_id,
                    text,
                    model,
                    sandbox,
                })
                .await;
            if result.is_ok() || matches!(&result, Err(SessionApplicationError::Conflict { .. })) {
                drafts.lock().await.remove(&session_id);
            }
            let receipt = result?;
            Ok(receipt.into())
        }
        WireOperation::SubmitImage {
            session_id,
            turn_id,
            model,
            request,
        } => {
            let handle = get_handle(&application, &drafts, &session_id).await?;
            let result = handle
                .submit_image(SubmitDirectImage {
                    turn_id,
                    model,
                    request,
                })
                .await;
            if result.is_ok() || matches!(&result, Err(SessionApplicationError::Conflict { .. })) {
                drafts.lock().await.remove(&session_id);
            }
            let receipt = result?;
            Ok(receipt.into())
        }
        WireOperation::Cancel {
            session_id,
            turn_id,
            reason,
        } => {
            let handle = get_handle(&application, &drafts, &session_id).await?;
            let result = handle.cancel(&turn_id, reason).await?;
            Ok(WireResponse::Cancel {
                accepted: result.accepted,
                already_terminal: result.already_terminal,
            })
        }
        WireOperation::AnswerApproval {
            session_id,
            approval_id,
            decision,
        } => {
            let handle = get_handle(&application, &drafts, &session_id).await?;
            Ok(WireResponse::ApprovalAnswer {
                accepted: handle.answer_approval(&approval_id, decision).await?,
            })
        }
        WireOperation::ListRecent { .. }
        | WireOperation::History { .. }
        | WireOperation::PendingApprovals { .. }
        | WireOperation::Subscribe { .. } => unreachable!("sequence handled before dispatch"),
    }
}

async fn create_draft(
    application: Arc<dyn SessionApplication>,
    drafts: Arc<UnpublishedDrafts>,
    draft_admission: Arc<Semaphore>,
    cwd: String,
    session_id: Option<SessionId>,
    agent_preset_id: Option<AgentPresetId>,
) -> rsi_session::Result<WireResponse> {
    let cwd = PathBuf::from(cwd);
    if !cwd.is_absolute() {
        return Err(SessionApplicationError::Invalid(
            "wire workspace path must be absolute".into(),
        ));
    }
    let expired = {
        let mut drafts = drafts.lock().await;
        take_expired_drafts(&mut drafts, tokio::time::Instant::now())
    };
    drop(expired);
    let admission = draft_admission
        .try_acquire_owned()
        .map_err(|_| SessionApplicationError::Capacity)?;
    let handle = application
        .create(CreateSession {
            cwd,
            session_id,
            agent_preset_id,
        })
        .await?;
    let header = handle.header().await?;
    let mut drafts = drafts.lock().await;
    let expired = take_expired_drafts(&mut drafts, tokio::time::Instant::now());
    if drafts.contains_key(header.session_id()) {
        drop(drafts);
        drop(expired);
        return Err(SessionApplicationError::Invalid(
            "an unpublished draft with this Session identity already exists".into(),
        ));
    }
    drafts.insert(
        header.session_id().clone(),
        UnpublishedDraft {
            handle,
            expires_at: unpublished_draft_deadline(),
            _admission: admission,
        },
    );
    drop(drafts);
    drop(expired);
    Ok(WireResponse::Session { header })
}

async fn serve_sequence(
    stream: &mut UnixStream,
    request_id: &str,
    application: Arc<dyn SessionApplication>,
    drafts: Arc<UnpublishedDrafts>,
    operation: WireOperation,
) -> Result<(), SessionHostError> {
    let result: rsi_session::Result<(WireResponse, Vec<WireItem>)> = match operation {
        WireOperation::ListRecent { after, limit } => {
            let cursor = after.map(|cursor| RecentSessionCursor {
                created_at_ms: cursor.created_at_ms,
                session_id: cursor.session_id,
            });
            application
                .list_recent(cursor.as_ref(), limit)
                .await
                .map(|page| {
                    (
                        WireResponse::RecentStart {
                            has_more: page.has_more,
                        },
                        page.sessions
                            .into_iter()
                            .map(|summary| WireItem::Session {
                                header: summary.header,
                            })
                            .collect(),
                    )
                })
        }
        WireOperation::History {
            session_id,
            exclusive_before_seq,
            limit,
        } => match get_handle(&application, &drafts, &session_id).await {
            Ok(handle) => handle
                .history_before(exclusive_before_seq, limit)
                .await
                .map(|page| {
                    (
                        WireResponse::HistoryStart {
                            before_seq: page.before_seq,
                            durable_seq: page.durable_seq,
                            has_more: page.has_more,
                        },
                        page.facts
                            .into_iter()
                            .map(|fact| WireItem::Fact {
                                session_id: session_id.clone(),
                                fact,
                            })
                            .collect(),
                    )
                }),
            Err(error) => Err(error),
        },
        WireOperation::PendingApprovals { session_id } => {
            match get_handle(&application, &drafts, &session_id).await {
                Ok(handle) => handle.pending_approvals().await.map(|requests| {
                    (
                        WireResponse::PendingApprovalsStart,
                        requests
                            .into_iter()
                            .map(|request| WireItem::Approval { request })
                            .collect(),
                    )
                }),
                Err(error) => Err(error),
            }
        }
        _ => unreachable!("only sequence operations reach sequence dispatch"),
    };

    let (response, items) = match result {
        Ok(result) => result,
        Err(error) => {
            return write_frame(
                stream,
                &ServerFrame::Response {
                    request_id: request_id.into(),
                    response: None,
                    error: Some(error.into()),
                },
            )
            .await;
        }
    };
    write_sequence_frames(stream, request_id, response, items).await
}

async fn write_sequence_frames(
    stream: &mut UnixStream,
    request_id: &str,
    response: WireResponse,
    items: Vec<WireItem>,
) -> Result<(), SessionHostError> {
    write_frame(
        stream,
        &ServerFrame::Response {
            request_id: request_id.into(),
            response: Some(response),
            error: None,
        },
    )
    .await?;
    for item in items {
        write_frame(
            stream,
            &ServerFrame::Item {
                request_id: request_id.into(),
                item,
            },
        )
        .await?;
    }
    write_frame(
        stream,
        &ServerFrame::End {
            request_id: request_id.into(),
            error: None,
        },
    )
    .await
}

impl From<TurnReceipt> for WireResponse {
    fn from(receipt: TurnReceipt) -> Self {
        Self::Receipt {
            session_id: receipt.session_id,
            turn_id: receipt.turn_id,
            accepted_seq: receipt.accepted_seq,
        }
    }
}

#[derive(Debug)]
enum SequenceContract {
    Recent { limit: usize },
    History { session_id: SessionId, limit: usize },
    PendingApprovals { session_id: SessionId },
}

impl SequenceContract {
    fn from_operation(operation: &WireOperation) -> rsi_session::Result<Self> {
        match operation {
            WireOperation::ListRecent { limit, .. } => Ok(Self::Recent { limit: *limit }),
            WireOperation::History {
                session_id, limit, ..
            } => Ok(Self::History {
                session_id: session_id.clone(),
                limit: *limit,
            }),
            WireOperation::PendingApprovals { session_id } => Ok(Self::PendingApprovals {
                session_id: session_id.clone(),
            }),
            _ => Err(SessionApplicationError::Backend(
                "Session Host sequence contract does not match its operation".into(),
            )),
        }
    }

    fn validate_start(&self, response: &WireResponse) -> rsi_session::Result<()> {
        if matches!(
            (self, response),
            (Self::Recent { .. }, WireResponse::RecentStart { .. })
                | (Self::History { .. }, WireResponse::HistoryStart { .. })
                | (
                    Self::PendingApprovals { .. },
                    WireResponse::PendingApprovalsStart
                )
        ) {
            Ok(())
        } else {
            Err(unexpected_response())
        }
    }

    fn admit_item(
        &self,
        current_items: usize,
        current_bytes: usize,
        item: &WireItem,
    ) -> rsi_session::Result<usize> {
        admit_sequence_item(current_items)?;
        let (maximum_items, item_bytes) = match (self, item) {
            (Self::Recent { limit }, WireItem::Session { header }) => (
                *limit,
                serde_json::to_vec(header)
                    .map_err(|error| SessionApplicationError::Backend(error.to_string()))?
                    .len(),
            ),
            (
                Self::History { session_id, limit },
                WireItem::Fact {
                    session_id: item_session_id,
                    fact,
                },
            ) if item_session_id == session_id => (*limit, fact.encoded_len()),
            (Self::PendingApprovals { session_id }, WireItem::Approval { request })
                if request.subject.session_id() == session_id.as_str() =>
            {
                (
                    MAXIMUM_SEQUENCE_ITEMS,
                    serde_json::to_vec(request)
                        .map_err(|error| SessionApplicationError::Backend(error.to_string()))?
                        .len(),
                )
            }
            _ => {
                return Err(SessionApplicationError::Backend(
                    "Session Host sequence item violates its operation contract".into(),
                ));
            }
        };
        if current_items >= maximum_items {
            return Err(SessionApplicationError::Backend(
                "Session Host sequence exceeds its operation count bound".into(),
            ));
        }
        let next_bytes = current_bytes.checked_add(item_bytes).ok_or_else(|| {
            SessionApplicationError::Backend("Session Host sequence byte count overflowed".into())
        })?;
        let maximum_bytes = match self {
            Self::Recent { limit } => limit
                .checked_mul(rsi_agent_session_protocol::MAXIMUM_SESSION_HEADER_BYTES)
                .ok_or_else(|| {
                    SessionApplicationError::Backend(
                        "Session Host recent sequence byte bound overflowed".into(),
                    )
                })?,
            Self::History { .. } | Self::PendingApprovals { .. } => MAXIMUM_STORE_FACT_PAGE_BYTES,
        };
        if next_bytes > maximum_bytes {
            return Err(SessionApplicationError::Backend(
                "Session Host sequence exceeds its aggregate byte bound".into(),
            ));
        }
        Ok(next_bytes)
    }
}

async fn get_handle(
    application: &Arc<dyn SessionApplication>,
    drafts: &UnpublishedDrafts,
    session_id: &SessionId,
) -> rsi_session::Result<Arc<dyn SessionHandle>> {
    let now = tokio::time::Instant::now();
    let mut drafts = drafts.lock().await;
    let expired = take_expired_drafts(&mut drafts, now);
    let handle = drafts.get_mut(session_id).map(|draft| {
        draft.expires_at = now + UNPUBLISHED_DRAFT_IDLE_TIMEOUT;
        Arc::clone(&draft.handle)
    });
    drop(drafts);
    drop(expired);
    if let Some(handle) = handle {
        return Ok(handle);
    }
    application.attach(session_id).await
}

fn unpublished_draft_deadline() -> tokio::time::Instant {
    tokio::time::Instant::now() + UNPUBLISHED_DRAFT_IDLE_TIMEOUT
}

async fn prune_expired_drafts(drafts: &UnpublishedDrafts) {
    let mut drafts = drafts.lock().await;
    let expired = take_expired_drafts(&mut drafts, tokio::time::Instant::now());
    drop(drafts);
    drop(expired);
}

fn take_expired_drafts(
    drafts: &mut BTreeMap<SessionId, UnpublishedDraft>,
    now: tokio::time::Instant,
) -> Vec<UnpublishedDraft> {
    let expired = drafts
        .iter()
        .filter(|(_, draft)| draft.expires_at <= now)
        .map(|(session_id, _)| session_id.clone())
        .collect::<Vec<_>>();
    expired
        .into_iter()
        .filter_map(|session_id| drafts.remove(&session_id))
        .collect()
}

async fn serve_subscription(
    stream: &mut UnixStream,
    request_id: &str,
    session_id: &SessionId,
    after_seq: u64,
    application: Arc<dyn SessionApplication>,
    drafts: Arc<UnpublishedDrafts>,
    shutdown: CancellationToken,
) -> Result<(), SessionHostError> {
    let handle = match get_handle(&application, &drafts, session_id).await {
        Ok(handle) => handle,
        Err(error) => {
            return write_frame(
                stream,
                &ServerFrame::Response {
                    request_id: request_id.into(),
                    response: None,
                    error: Some(error.into()),
                },
            )
            .await;
        }
    };
    let mut observation = match handle.subscribe(after_seq).await {
        Ok(observation) => observation,
        Err(error) => {
            return write_frame(
                stream,
                &ServerFrame::Response {
                    request_id: request_id.into(),
                    response: None,
                    error: Some(error.into()),
                },
            )
            .await;
        }
    };
    write_frame(
        stream,
        &ServerFrame::Response {
            request_id: request_id.into(),
            response: Some(WireResponse::Subscribed),
            error: None,
        },
    )
    .await?;
    loop {
        let next = tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                return write_frame(stream, &ServerFrame::End {
                    request_id: request_id.into(),
                    error: Some(WireError::ShuttingDown),
                }).await;
            }
            next = observation.next() => next,
        };
        match next {
            Some(Ok(update)) => {
                write_frame(
                    stream,
                    &ServerFrame::Event {
                        request_id: request_id.into(),
                        session_id: session_id.clone(),
                        update: update.into(),
                    },
                )
                .await?;
            }
            Some(Err(error)) => {
                return write_frame(
                    stream,
                    &ServerFrame::End {
                        request_id: request_id.into(),
                        error: Some(WireError::Backend {
                            message: error.to_string(),
                        }),
                    },
                )
                .await;
            }
            None => {
                return write_frame(
                    stream,
                    &ServerFrame::End {
                        request_id: request_id.into(),
                        error: None,
                    },
                )
                .await;
            }
        }
    }
}

/// Remote adapter for the same product-level Session interface.
#[derive(Debug)]
pub struct UdsSessionApplication {
    socket: PathBuf,
    launch_key: String,
    host_epoch: HostEpoch,
    next_request: Arc<AtomicU64>,
    frame_budget: FrameReadBudget,
}

impl UdsSessionApplication {
    /// Probes and validates one exact daemon generation before returning an adapter.
    pub async fn connect(
        socket: impl Into<PathBuf>,
        launch_key: impl Into<String>,
        host_epoch: HostEpoch,
    ) -> rsi_session::Result<Self> {
        let application = Self {
            socket: socket.into(),
            launch_key: launch_key.into(),
            host_epoch,
            next_request: Arc::new(AtomicU64::new(1)),
            frame_budget: FrameReadBudget::default(),
        };
        validate_launch_key(&application.launch_key).map_err(host_as_session_error)?;
        let mut stream = application.connect_stream().await?;
        stream.shutdown().await.map_err(io_as_session_error)?;
        Ok(application)
    }

    async fn connect_stream(&self) -> rsi_session::Result<UnixStream> {
        let expected_product_build = session_host_product_build().map_err(host_as_session_error)?;
        let mut stream = tokio::time::timeout(CONNECT_TIMEOUT, UnixStream::connect(&self.socket))
            .await
            .map_err(|_| SessionApplicationError::Backend("Session Host connect timed out".into()))?
            .map_err(io_as_session_error)?;
        write_frame(
            &mut stream,
            &ClientFrame::Hello {
                protocol_epoch: SESSION_HOST_PROTOCOL_EPOCH,
                product_build: expected_product_build.into(),
                launch_key: self.launch_key.clone(),
                host_epoch: self.host_epoch.clone(),
            },
        )
        .await
        .map_err(host_as_session_error)?;
        match read_frame_with_timeout::<_, ServerFrame>(
            &mut stream,
            MAXIMUM_HANDSHAKE_FRAME_BYTES,
            &self.frame_budget,
            HANDSHAKE_READ_TIMEOUT,
            "server handshake",
        )
        .await
        .map_err(host_as_session_error)?
        {
            ServerFrame::HelloOk {
                protocol_epoch,
                product_build,
                launch_key,
                host_epoch,
            } if protocol_epoch == SESSION_HOST_PROTOCOL_EPOCH
                && product_build == expected_product_build
                && launch_key == self.launch_key
                && host_epoch == self.host_epoch =>
            {
                Ok(stream)
            }
            ServerFrame::HelloRejected { reason } => Err(SessionApplicationError::Backend(
                format!("Session Host handshake rejected: {reason}"),
            )),
            _ => Err(SessionApplicationError::Backend(
                "Session Host returned an incompatible handshake".into(),
            )),
        }
    }

    fn request_id(&self) -> String {
        format!(
            "request-{}",
            self.next_request.fetch_add(1, Ordering::Relaxed)
        )
    }

    async fn call(&self, operation: WireOperation) -> rsi_session::Result<WireResponse> {
        let mut stream = self.connect_stream().await?;
        let request_id = self.request_id();
        write_frame(
            &mut stream,
            &ClientFrame::Request {
                request_id: request_id.clone(),
                operation,
            },
        )
        .await
        .map_err(host_as_session_error)?;
        match read_frame_with_timeout::<_, ServerFrame>(
            &mut stream,
            MAXIMUM_FRAME_BYTES,
            &self.frame_budget,
            RESPONSE_READ_TIMEOUT,
            "server response",
        )
        .await
        .map_err(host_as_session_error)?
        {
            ServerFrame::Response {
                request_id: response_id,
                response,
                error,
            } if response_id == request_id => decode_response(response, error),
            _ => Err(SessionApplicationError::Backend(
                "Session Host response did not match its request".into(),
            )),
        }
    }

    async fn sequence(
        &self,
        operation: WireOperation,
    ) -> rsi_session::Result<(WireResponse, Vec<WireItem>)> {
        let contract = SequenceContract::from_operation(&operation)?;
        let mut stream = self.connect_stream().await?;
        let request_id = self.request_id();
        write_frame(
            &mut stream,
            &ClientFrame::Request {
                request_id: request_id.clone(),
                operation,
            },
        )
        .await
        .map_err(host_as_session_error)?;
        let response = match read_frame_with_timeout::<_, ServerFrame>(
            &mut stream,
            MAXIMUM_FRAME_BYTES,
            &self.frame_budget,
            RESPONSE_READ_TIMEOUT,
            "server sequence response",
        )
        .await
        .map_err(host_as_session_error)?
        {
            ServerFrame::Response {
                request_id: response_id,
                response,
                error,
            } if response_id == request_id => decode_response(response, error)?,
            _ => {
                return Err(SessionApplicationError::Backend(
                    "Session Host sequence response did not match its request".into(),
                ));
            }
        };
        contract.validate_start(&response)?;
        let mut items = Vec::new();
        let mut retained_bytes = 0_usize;
        loop {
            match read_frame_with_timeout::<_, ServerFrame>(
                &mut stream,
                MAXIMUM_FRAME_BYTES,
                &self.frame_budget,
                RESPONSE_READ_TIMEOUT,
                "server sequence item",
            )
            .await
            .map_err(host_as_session_error)?
            {
                ServerFrame::Item {
                    request_id: response_id,
                    item,
                } if response_id == request_id => {
                    retained_bytes = contract.admit_item(items.len(), retained_bytes, &item)?;
                    items.push(item);
                }
                ServerFrame::End {
                    request_id: response_id,
                    error: None,
                } if response_id == request_id => return Ok((response, items)),
                ServerFrame::End {
                    request_id: response_id,
                    error: Some(error),
                } if response_id == request_id => return Err(error.into()),
                _ => {
                    return Err(SessionApplicationError::Backend(
                        "Session Host sequence item did not match its request".into(),
                    ));
                }
            }
        }
    }
}

fn admit_sequence_item(current_items: usize) -> rsi_session::Result<()> {
    if current_items >= MAXIMUM_SEQUENCE_ITEMS {
        return Err(SessionApplicationError::Backend(format!(
            "Session Host sequence exceeds its {MAXIMUM_SEQUENCE_ITEMS}-item bound"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_fact() -> SessionFact {
        SessionFact::new(
            1,
            1,
            rsi_agent_session_protocol::SessionFactBody::TurnAccepted {
                turn_id: TurnId::new("turn-one").unwrap(),
                text: "hello".into(),
                model: None,
                sandbox: SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap()
    }

    #[tokio::test]
    async fn frame_budget_is_acquired_before_reading_the_declared_body() {
        let budget = FrameReadBudget::new(4);
        let (mut first_writer, mut first_reader) = tokio::io::duplex(32);
        let (mut second_writer, mut second_reader) = tokio::io::duplex(32);
        first_writer.write_u32(4).await.unwrap();
        second_writer.write_u32(4).await.unwrap();
        second_writer.write_all(b"null").await.unwrap();

        let first = tokio::spawn({
            let budget = budget.clone();
            async move { read_frame::<_, Option<()>>(&mut first_reader, 4, &budget).await }
        });
        tokio::task::yield_now().await;
        let second = tokio::spawn({
            let budget = budget.clone();
            async move { read_frame::<_, Option<()>>(&mut second_reader, 4, &budget).await }
        });
        tokio::task::yield_now().await;
        assert!(!second.is_finished());

        first_writer.write_all(b"null").await.unwrap();
        assert_eq!(first.await.unwrap().unwrap(), None);
        assert_eq!(second.await.unwrap().unwrap(), None);
    }

    #[tokio::test]
    async fn frame_budget_is_released_after_decode_error_and_read_timeout() {
        let budget = FrameReadBudget::new(4);

        let (mut invalid_writer, mut invalid_reader) = tokio::io::duplex(32);
        invalid_writer.write_u32(4).await.unwrap();
        invalid_writer.write_all(b"xxxx").await.unwrap();
        assert!(
            read_frame::<_, Option<()>>(&mut invalid_reader, 4, &budget)
                .await
                .is_err()
        );

        let (mut stalled_writer, mut stalled_reader) = tokio::io::duplex(32);
        stalled_writer.write_u32(4).await.unwrap();
        assert!(
            read_frame_with_timeout::<_, Option<()>>(
                &mut stalled_reader,
                4,
                &budget,
                Duration::from_millis(1),
                "test",
            )
            .await
            .is_err()
        );

        let (mut valid_writer, mut valid_reader) = tokio::io::duplex(32);
        valid_writer.write_u32(4).await.unwrap();
        valid_writer.write_all(b"null").await.unwrap();
        let decoded = tokio::time::timeout(
            Duration::from_secs(1),
            read_frame::<_, Option<()>>(&mut valid_reader, 4, &budget),
        )
        .await
        .expect("failed reads retained the complete frame ledger")
        .unwrap();
        assert_eq!(decoded, None);
    }

    #[tokio::test(start_paused = true)]
    async fn subscription_frame_body_is_bounded_after_its_length_arrives() {
        let budget = FrameReadBudget::new(4);
        let (mut writer, mut reader) = tokio::io::duplex(32);
        writer.write_u32(4).await.unwrap();
        let read_budget = budget.clone();
        let read = tokio::spawn(async move {
            read_subscription_frame::<_, Option<()>>(&mut reader, 4, &read_budget).await
        });

        tokio::task::yield_now().await;
        tokio::time::advance(RESPONSE_READ_TIMEOUT + Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert!(
            read.is_finished(),
            "a declared subscription frame retained decoder admission without a deadline"
        );
        assert!(matches!(
            read.await.unwrap(),
            Err(SessionHostError::Io(message)) if message.contains("subscription frame body read timed out")
        ));

        let (mut valid_writer, mut valid_reader) = tokio::io::duplex(32);
        valid_writer.write_u32(4).await.unwrap();
        valid_writer.write_all(b"null").await.unwrap();
        assert_eq!(
            read_subscription_frame::<_, Option<()>>(&mut valid_reader, 4, &budget)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test(start_paused = true)]
    async fn subscription_idle_wait_starts_no_body_deadline() {
        let budget = FrameReadBudget::new(4);
        let (mut writer, mut reader) = tokio::io::duplex(32);
        let read = tokio::spawn(async move {
            read_subscription_frame::<_, Option<()>>(&mut reader, 4, &budget).await
        });

        tokio::time::advance(RESPONSE_READ_TIMEOUT + Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert!(!read.is_finished());
        writer.write_u32(4).await.unwrap();
        writer.write_all(b"null").await.unwrap();
        assert_eq!(read.await.unwrap().unwrap(), None);
    }

    #[test]
    fn history_sequence_rejects_a_fact_for_another_session() {
        let expected = SessionId::new("expected-session").unwrap();
        let contract = SequenceContract::History {
            session_id: expected,
            limit: 1,
        };
        let item = WireItem::Fact {
            session_id: SessionId::new("other-session").unwrap(),
            fact: test_fact(),
        };

        assert!(contract.admit_item(0, 0, &item).is_err());
    }

    #[test]
    fn subscription_rejects_an_event_for_another_session() {
        let expected = SessionId::new("expected-session").unwrap();
        let frame = ServerFrame::Event {
            request_id: "request-one".into(),
            session_id: SessionId::new("other-session").unwrap(),
            update: WireUpdate::Fact {
                fact: Box::new(test_fact()),
                durable_seq: 1,
            },
        };

        assert!(decode_subscription_frame(frame, "request-one", &expected).is_err());
    }

    #[test]
    fn sequence_item_count_is_bounded_independently_of_frame_size() {
        assert!(admit_sequence_item(MAXIMUM_SEQUENCE_ITEMS - 1).is_ok());
        assert!(matches!(
            admit_sequence_item(MAXIMUM_SEQUENCE_ITEMS),
            Err(SessionApplicationError::Backend(message))
                if message.contains("1024-item bound")
        ));
    }
}

#[async_trait]
impl SessionApplication for UdsSessionApplication {
    async fn create(&self, request: CreateSession) -> rsi_session::Result<Arc<dyn SessionHandle>> {
        let expected_session_id = request.session_id.clone();
        let cwd = canonical_workspace_directory(&request.cwd).await?;
        let cwd = cwd.to_str().ok_or_else(|| {
            SessionApplicationError::Invalid("workspace path is not UTF-8".into())
        })?;
        let response = self
            .call(WireOperation::Create {
                cwd: cwd.into(),
                session_id: request.session_id,
                agent_preset_id: request.agent_preset_id,
            })
            .await?;
        self.handle_response(response, expected_session_id.as_ref())
    }

    async fn attach(&self, session_id: &SessionId) -> rsi_session::Result<Arc<dyn SessionHandle>> {
        let response = self
            .call(WireOperation::Attach {
                session_id: session_id.clone(),
            })
            .await?;
        self.handle_response(response, Some(session_id))
    }

    async fn list_recent(
        &self,
        after: Option<&RecentSessionCursor>,
        limit: usize,
    ) -> rsi_session::Result<RecentSessionPage> {
        let (response, items) = self
            .sequence(WireOperation::ListRecent {
                after: after.map(|cursor| WireRecentCursor {
                    created_at_ms: cursor.created_at_ms,
                    session_id: cursor.session_id.clone(),
                }),
                limit,
            })
            .await?;
        match response {
            WireResponse::RecentStart { has_more } => Ok(RecentSessionPage {
                sessions: items
                    .into_iter()
                    .map(|item| match item {
                        WireItem::Session { header } => Ok(SessionSummary { header }),
                        _ => Err(unexpected_response()),
                    })
                    .collect::<rsi_session::Result<Vec<_>>>()?,
                has_more,
            }),
            _ => Err(unexpected_response()),
        }
    }
}

impl UdsSessionApplication {
    fn handle_response(
        &self,
        response: WireResponse,
        expected_session_id: Option<&SessionId>,
    ) -> rsi_session::Result<Arc<dyn SessionHandle>> {
        match response {
            WireResponse::Session { header }
                if expected_session_id.is_none_or(|expected| expected == header.session_id()) =>
            {
                Ok(Arc::new(UdsSessionHandle {
                    application: Arc::new(self.clone_inner()),
                    header,
                }))
            }
            WireResponse::Session { .. } => Err(SessionApplicationError::Backend(
                "Session Host returned a Header for a different Session identity".into(),
            )),
            _ => Err(unexpected_response()),
        }
    }

    fn clone_inner(&self) -> Self {
        Self {
            socket: self.socket.clone(),
            launch_key: self.launch_key.clone(),
            host_epoch: self.host_epoch.clone(),
            next_request: Arc::clone(&self.next_request),
            frame_budget: self.frame_budget.clone(),
        }
    }
}

#[derive(Debug)]
struct UdsSessionHandle {
    application: Arc<UdsSessionApplication>,
    header: SessionHeader,
}

#[async_trait]
impl SessionHandle for UdsSessionHandle {
    async fn header(&self) -> rsi_session::Result<SessionHeader> {
        Ok(self.header.clone())
    }

    async fn submit_text(&self, request: SubmitText) -> rsi_session::Result<TurnReceipt> {
        let turn_id = request.turn_id;
        decode_receipt(
            self.application
                .call(WireOperation::SubmitText {
                    session_id: self.header.session_id().clone(),
                    turn_id: turn_id.clone(),
                    text: request.text,
                    model: request.model,
                    sandbox: request.sandbox,
                })
                .await?,
            self.header.session_id(),
            &turn_id,
        )
    }

    async fn submit_image(&self, request: SubmitDirectImage) -> rsi_session::Result<TurnReceipt> {
        let turn_id = request.turn_id;
        decode_receipt(
            self.application
                .call(WireOperation::SubmitImage {
                    session_id: self.header.session_id().clone(),
                    turn_id: turn_id.clone(),
                    model: request.model,
                    request: request.request,
                })
                .await?,
            self.header.session_id(),
            &turn_id,
        )
    }

    async fn cancel(
        &self,
        turn_id: &TurnId,
        reason: Option<String>,
    ) -> rsi_session::Result<CancelResult> {
        match self
            .application
            .call(WireOperation::Cancel {
                session_id: self.header.session_id().clone(),
                turn_id: turn_id.clone(),
                reason,
            })
            .await?
        {
            WireResponse::Cancel {
                accepted,
                already_terminal,
            } => Ok(CancelResult {
                accepted,
                already_terminal,
            }),
            _ => Err(unexpected_response()),
        }
    }

    async fn history_before(
        &self,
        exclusive_before_seq: Option<u64>,
        limit: usize,
    ) -> rsi_session::Result<SessionHistoryPage> {
        let (response, items) = self
            .application
            .sequence(WireOperation::History {
                session_id: self.header.session_id().clone(),
                exclusive_before_seq,
                limit,
            })
            .await?;
        match response {
            WireResponse::HistoryStart {
                before_seq,
                durable_seq,
                has_more,
            } => {
                let page = StoreBackwardFactPage {
                    before_seq,
                    facts: items
                        .into_iter()
                        .map(|item| match item {
                            WireItem::Fact { fact, .. } => Ok(fact),
                            _ => Err(unexpected_response()),
                        })
                        .collect::<rsi_session::Result<Vec<_>>>()?,
                    durable_seq,
                    has_more,
                };
                page.validate().map_err(|error| {
                    SessionApplicationError::Backend(format!(
                        "Session Host history page violates its bound: {error}"
                    ))
                })?;
                Ok(SessionHistoryPage {
                    before_seq: page.before_seq,
                    facts: page.facts,
                    durable_seq: page.durable_seq,
                    has_more: page.has_more,
                })
            }
            _ => Err(unexpected_response()),
        }
    }

    async fn subscribe(&self, after_seq: u64) -> rsi_session::Result<TurnObservation> {
        let mut stream = self.application.connect_stream().await?;
        let request_id = self.application.request_id();
        write_frame(
            &mut stream,
            &ClientFrame::Request {
                request_id: request_id.clone(),
                operation: WireOperation::Subscribe {
                    session_id: self.header.session_id().clone(),
                    after_seq,
                },
            },
        )
        .await
        .map_err(host_as_session_error)?;
        match read_frame_with_timeout::<_, ServerFrame>(
            &mut stream,
            MAXIMUM_FRAME_BYTES,
            &self.application.frame_budget,
            RESPONSE_READ_TIMEOUT,
            "subscription response",
        )
        .await
        .map_err(host_as_session_error)?
        {
            ServerFrame::Response {
                request_id: response_id,
                response: Some(WireResponse::Subscribed),
                error: None,
            } if response_id == request_id => {}
            ServerFrame::Response {
                request_id: response_id,
                response: None,
                error: Some(error),
            } if response_id == request_id => return Err(error.into()),
            _ => return Err(unexpected_response()),
        }
        let frame_budget = self.application.frame_budget.clone();
        let expected_session_id = self.header.session_id().clone();
        Ok(Box::pin(async_stream::stream! {
            loop {
                match read_subscription_frame::<_, ServerFrame>(
                    &mut stream,
                    MAXIMUM_FRAME_BYTES,
                    &frame_budget,
                ).await {
                    Ok(frame) => match decode_subscription_frame(
                        frame,
                        &request_id,
                        &expected_session_id,
                    ) {
                        Ok(DecodedSubscriptionFrame::Update(update)) => yield Ok(update),
                        Ok(DecodedSubscriptionFrame::End(error)) => {
                            if let Some(error) = error {
                                yield Err(wire_as_turn_error(error));
                            }
                            break;
                        }
                        Err(error) => {
                            yield Err(error);
                            break;
                        }
                    },
                    Err(error) => {
                        yield Err(TurnError::Invariant(error.to_string()));
                        break;
                    }
                }
            }
        }))
    }

    async fn pending_approvals(&self) -> rsi_session::Result<Vec<ApprovalRequest>> {
        let (response, items) = self
            .application
            .sequence(WireOperation::PendingApprovals {
                session_id: self.header.session_id().clone(),
            })
            .await?;
        match response {
            WireResponse::PendingApprovalsStart => items
                .into_iter()
                .map(|item| match item {
                    WireItem::Approval { request } => Ok(request),
                    _ => Err(unexpected_response()),
                })
                .collect(),
            _ => Err(unexpected_response()),
        }
    }

    async fn answer_approval(
        &self,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> rsi_session::Result<bool> {
        match self
            .application
            .call(WireOperation::AnswerApproval {
                session_id: self.header.session_id().clone(),
                approval_id: approval_id.into(),
                decision,
            })
            .await?
        {
            WireResponse::ApprovalAnswer { accepted } => Ok(accepted),
            _ => Err(unexpected_response()),
        }
    }
}

fn decode_receipt(
    response: WireResponse,
    expected_session_id: &SessionId,
    expected_turn_id: &TurnId,
) -> rsi_session::Result<TurnReceipt> {
    match response {
        WireResponse::Receipt {
            session_id,
            turn_id,
            accepted_seq,
        } if &session_id == expected_session_id && &turn_id == expected_turn_id => {
            Ok(TurnReceipt {
                session_id,
                turn_id,
                accepted_seq,
            })
        }
        WireResponse::Receipt { .. } => Err(SessionApplicationError::Backend(
            "Session Host returned a receipt for a different Session or Turn identity".into(),
        )),
        _ => Err(unexpected_response()),
    }
}

fn decode_response(
    response: Option<WireResponse>,
    error: Option<WireError>,
) -> rsi_session::Result<WireResponse> {
    match (response, error) {
        (Some(response), None) => Ok(response),
        (None, Some(error)) => Err(error.into()),
        _ => Err(SessionApplicationError::Backend(
            "Session Host returned an invalid response envelope".into(),
        )),
    }
}

fn unexpected_response() -> SessionApplicationError {
    SessionApplicationError::Backend("Session Host returned the wrong response variant".into())
}

fn wire_as_turn_error(error: WireError) -> TurnError {
    match error {
        WireError::ShuttingDown => TurnError::ShuttingDown,
        other => TurnError::Invariant(SessionApplicationError::from(other).to_string()),
    }
}

enum DecodedSubscriptionFrame {
    Update(TurnUpdate),
    End(Option<WireError>),
}

fn decode_subscription_frame(
    frame: ServerFrame,
    expected_request_id: &str,
    expected_session_id: &SessionId,
) -> Result<DecodedSubscriptionFrame, TurnError> {
    match frame {
        ServerFrame::Event {
            request_id,
            session_id,
            update,
        } if request_id == expected_request_id && &session_id == expected_session_id => {
            Ok(DecodedSubscriptionFrame::Update(update.into()))
        }
        ServerFrame::End { request_id, error } if request_id == expected_request_id => {
            Ok(DecodedSubscriptionFrame::End(error))
        }
        _ => Err(TurnError::Invariant(
            "Session Host stream response did not match its request".into(),
        )),
    }
}

async fn read_frame<R, T>(
    reader: &mut R,
    maximum_bytes: usize,
    budget: &FrameReadBudget,
) -> Result<T, SessionHostError>
where
    R: AsyncRead + Unpin,
    T: serde::de::DeserializeOwned,
{
    let length = read_frame_length(reader, maximum_bytes).await?;
    read_frame_body(reader, length, budget).await
}

async fn read_frame_length<R>(
    reader: &mut R,
    maximum_bytes: usize,
) -> Result<usize, SessionHostError>
where
    R: AsyncRead + Unpin,
{
    let length = reader.read_u32().await.map_err(io_error)? as usize;
    if length == 0 || length > maximum_bytes {
        return Err(SessionHostError::Invalid(format!(
            "frame length must be within 1..={maximum_bytes} bytes"
        )));
    }
    Ok(length)
}

async fn read_frame_body<R, T>(
    reader: &mut R,
    length: usize,
    budget: &FrameReadBudget,
) -> Result<T, SessionHostError>
where
    R: AsyncRead + Unpin,
    T: serde::de::DeserializeOwned,
{
    let _admission = budget.acquire(length).await?;
    let mut bytes = vec![0_u8; length];
    reader.read_exact(&mut bytes).await.map_err(io_error)?;
    serde_json::from_slice(&bytes).map_err(|error| SessionHostError::Invalid(error.to_string()))
}

async fn read_frame_with_timeout<R, T>(
    reader: &mut R,
    maximum_bytes: usize,
    budget: &FrameReadBudget,
    timeout: Duration,
    phase: &str,
) -> Result<T, SessionHostError>
where
    R: AsyncRead + Unpin,
    T: serde::de::DeserializeOwned,
{
    tokio::time::timeout(timeout, read_frame(reader, maximum_bytes, budget))
        .await
        .map_err(|_| SessionHostError::Io(format!("Session Host {phase} read timed out")))?
}

async fn read_subscription_frame<R, T>(
    reader: &mut R,
    maximum_bytes: usize,
    budget: &FrameReadBudget,
) -> Result<T, SessionHostError>
where
    R: AsyncRead + Unpin,
    T: serde::de::DeserializeOwned,
{
    let length = read_frame_length(reader, maximum_bytes).await?;
    tokio::time::timeout(
        RESPONSE_READ_TIMEOUT,
        read_frame_body(reader, length, budget),
    )
    .await
    .map_err(|_| {
        SessionHostError::Io("Session Host subscription frame body read timed out".into())
    })?
}

async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<(), SessionHostError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let bytes =
        serde_json::to_vec(value).map_err(|error| SessionHostError::Invalid(error.to_string()))?;
    if bytes.is_empty() || bytes.len() > MAXIMUM_FRAME_BYTES {
        return Err(SessionHostError::Invalid(format!(
            "encoded frame length must be within 1..={MAXIMUM_FRAME_BYTES} bytes"
        )));
    }
    let length = u32::try_from(bytes.len())
        .map_err(|_| SessionHostError::Invalid("frame length exceeds u32".into()))?;
    tokio::time::timeout(WRITE_TIMEOUT, async {
        writer.write_u32(length).await?;
        writer.write_all(&bytes).await?;
        writer.flush().await
    })
    .await
    .map_err(|_| SessionHostError::Io("Session Host frame write timed out".into()))?
    .map_err(io_error)
}

fn validate_request_id(value: &str) -> Result<(), SessionHostError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(SessionHostError::Invalid(
            "request identity is empty, oversized, or malformed".into(),
        ));
    }
    Ok(())
}

fn create_private_runtime_directory(path: &Path) -> Result<(), SessionHostError> {
    let parent = path.parent().ok_or_else(|| {
        SessionHostError::Invalid("Session Host runtime path has no parent".into())
    })?;
    if parent.file_name().is_some_and(|name| name == "rsi") {
        let runtime_root = parent.parent().ok_or_else(|| {
            SessionHostError::Invalid("Session Host runtime root is missing".into())
        })?;
        fs::create_dir_all(runtime_root).map_err(io_error)?;
        validate_effective_user_directory(runtime_root, "Session Host runtime root")?;
        create_directory(parent)?;
        validate_effective_user_directory(parent, "Session Host runtime parent")?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
        create_directory(path)?;
    } else {
        fs::create_dir_all(path).map_err(io_error)?;
    }
    validate_effective_user_directory(path, "Session Host runtime path")?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(io_error)
}

fn create_directory(path: &Path) -> Result<(), SessionHostError> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

fn validate_effective_user_directory(path: &Path, label: &str) -> Result<(), SessionHostError> {
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SessionHostError::Invalid(format!(
            "{label} is not a real directory"
        )));
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(SessionHostError::Invalid(format!(
            "{label} is not owned by the effective user"
        )));
    }
    Ok(())
}

fn remove_stale_socket_after_failed_probe(path: &Path) -> Result<(), SessionHostError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error(error)),
    };
    if !metadata.file_type().is_socket() {
        return Err(SessionHostError::Invalid(
            "existing Session Host endpoint is not a socket".into(),
        ));
    }
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Err(SessionHostError::OwnerActive),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) =>
        {
            let current = fs::symlink_metadata(path).map_err(io_error)?;
            if current.file_type().is_socket()
                && current.dev() == metadata.dev()
                && current.ino() == metadata.ino()
            {
                fs::remove_file(path).map_err(io_error)
            } else {
                Err(SessionHostError::Invalid(
                    "Session Host endpoint changed during stale probe".into(),
                ))
            }
        }
        Err(error) => Err(io_error(error)),
    }
}

#[allow(clippy::needless_pass_by_value)] // Kept as direct I/O `map_err` adapters.
fn io_error(error: io::Error) -> SessionHostError {
    SessionHostError::Io(error.to_string())
}

#[allow(clippy::needless_pass_by_value)]
fn io_as_session_error(error: io::Error) -> SessionApplicationError {
    SessionApplicationError::Backend(error.to_string())
}

#[allow(clippy::needless_pass_by_value)]
fn host_as_session_error(error: SessionHostError) -> SessionApplicationError {
    SessionApplicationError::Backend(error.to_string())
}
