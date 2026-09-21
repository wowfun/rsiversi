//! Bounded agent-side dispatch; the application backend owns Session semantics.

use crate::{Error, Peer, PeerHandle};
use async_trait::async_trait;
use rsi_acp_protocol::{Message, schema};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::future::Future as _;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use std::task::Poll;
use std::time::Duration;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// Redacted application errors converted to standard JSON-RPC failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    /// Invalid or unsupported method parameters.
    Parameters,
    /// The Session is missing or not owned by this ACP backend.
    NotFound,
    /// Explicitly unsupported stable method.
    Unsupported,
    /// Initialization has not completed.
    Uninitialized,
    /// An operation is already active or capacity is exhausted.
    Busy,
    /// A control operation exceeded its deadline; its owner retains cleanup.
    Timeout,
    /// Complete cleanup or backend success could not be established.
    Backend,
}
impl Failure {
    fn value(self) -> Value {
        let (code, message) = match self {
            Self::Parameters => (-32602, "Invalid parameters"),
            Self::Unsupported => (-32601, "Method not supported"),
            Self::NotFound => (-32001, "Session unavailable"),
            Self::Uninitialized => (-32002, "Initialize first"),
            Self::Busy => (-32003, "Operation unavailable while busy"),
            Self::Timeout => (-32004, "Control operation timed out"),
            Self::Backend => (-32603, "Backend operation could not complete"),
        };
        json!({"code":code,"message":message})
    }
}

/// ACP application owner. Implementations retain native work independently of handlers.
#[async_trait]
pub trait AgentBackend: std::fmt::Debug + Send + Sync + 'static {
    /// Negotiates only capabilities this backend actually implements.
    async fn initialize(
        &self,
        request: schema::InitializeRequest,
    ) -> Result<schema::InitializeResponse, Failure>;
    /// Prepares MCP and composition before publishing a new native Session.
    async fn new_session(
        &self,
        request: schema::NewSessionRequest,
    ) -> Result<schema::NewSessionResponse, Failure>;
    /// Replays the full durable frozen horizon before returning.
    async fn load(
        &self,
        request: schema::LoadSessionRequest,
        peer: PeerHandle,
    ) -> Result<schema::LoadSessionResponse, Failure>;
    /// Restores execution context without replaying conversation updates.
    async fn resume(
        &self,
        request: schema::ResumeSessionRequest,
    ) -> Result<schema::ResumeSessionResponse, Failure>;
    /// Lists only this backend's ACP-owned native Sessions.
    async fn list(
        &self,
        request: schema::ListSessionsRequest,
    ) -> Result<schema::ListSessionsResponse, Failure>;
    /// Submits once, relays updates and permissions, and awaits truthful settlement.
    /// Admit or reject synchronously before the first await; first polls follow wire order.
    async fn prompt(
        &self,
        request: schema::PromptRequest,
        peer: PeerHandle,
    ) -> Result<schema::PromptResponse, Failure>;
    /// Cancels native work. The outstanding prompt responds only after settlement.
    async fn cancel(&self, request: schema::CancelNotification) -> Result<(), Failure>;
    /// Closes one owned Session and its session-scoped resources.
    async fn close(
        &self,
        request: schema::CloseSessionRequest,
    ) -> Result<schema::CloseSessionResponse, Failure>;
    /// Stops admission, cancels all work and settles session-scoped resources.
    async fn shutdown(&self) -> Result<(), Failure>;
}

fn parse<T: DeserializeOwned>(value: Value) -> Result<T, Failure> {
    serde_json::from_value(value).map_err(|_| Failure::Parameters)
}
fn encode(value: &impl Serialize) -> Result<Value, Failure> {
    serde_json::to_value(value).map_err(|_| Failure::Backend)
}

async fn dispatch(
    backend: &dyn AgentBackend,
    peer: PeerHandle,
    initialized: &AtomicU8,
    method: &str,
    params: Value,
) -> Result<Value, Failure> {
    if method == "initialize" {
        rsi_acp_protocol::validate_initialize(&params).map_err(|_| Failure::Parameters)?;
        let request = parse(params)?;
        if initialized
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(Failure::Busy);
        }
        let result = tokio::time::timeout(Duration::from_secs(10), backend.initialize(request))
            .await
            .map_err(|_| Failure::Timeout)
            .and_then(std::convert::identity);
        initialized.store(if result.is_ok() { 2 } else { 0 }, Ordering::Release);
        return encode(&result?);
    }
    if initialized.load(Ordering::Acquire) != 2 {
        return Err(Failure::Uninitialized);
    }
    match method {
        "session/new" | "session/load" | "session/resume" => {
            rsi_acp_protocol::validate_session_setup(&params, method != "session/new")
                .map_err(|_| Failure::Parameters)?;
        }
        "session/prompt" => {
            rsi_acp_protocol::validate_prompt(&params).map_err(|_| Failure::Parameters)?;
        }
        "session/close" => {
            rsi_acp_protocol::validate_session_id(&params).map_err(|_| Failure::Parameters)?;
        }
        "session/list" => {
            rsi_acp_protocol::validate_list(&params).map_err(|_| Failure::Parameters)?;
        }
        _ => {}
    }
    let operation = async {
        match method {
            "session/new" => encode(&backend.new_session(parse(params)?).await?),
            "session/load" => {
                let response = backend.load(parse(params)?, peer.clone()).await?;
                peer.drain().await.map_err(|_| Failure::Backend)?;
                encode(&response)
            }
            "session/resume" => encode(&backend.resume(parse(params)?).await?),
            "session/list" => encode(&backend.list(parse(params)?).await?),
            "session/prompt" => encode(&backend.prompt(parse(params)?, peer).await?),
            "session/close" => encode(&backend.close(parse(params)?).await?),
            _ => Err(Failure::Unsupported),
        }
    };
    if method == "session/prompt" {
        operation.await
    } else {
        tokio::time::timeout(Duration::from_secs(30), operation)
            .await
            .map_err(|_| Failure::Timeout)?
    }
}

/// Runs one protocol connection, joining handler tasks and native backend cleanup.
/// No prompts are automatically retried after handler or transport loss.
pub async fn run(
    mut peer: Peer,
    backend: Arc<dyn AgentBackend>,
    cancellation: CancellationToken,
) -> Result<(), Error> {
    let initialized = Arc::new(AtomicU8::new(0));
    let mut tasks = JoinSet::new();
    let mut healthy = true;
    loop {
        tokio::select! { biased;
            () = cancellation.cancelled() => break,
            result = tasks.join_next(), if !tasks.is_empty() => {
                if !matches!(result, Some(Ok(Ok(())))) { healthy = false; break; }
            }
            incoming = peer.next() => {
                let Some(incoming) = incoming else { break; };
                if tasks.len() == rsi_acp_protocol::MAX_PENDING { healthy = false; break; }
                let backend = backend.clone();
                let port = peer.handle();
                let initialized = initialized.clone();
                let mut handling = Box::pin(async move {
                    // Keep the incoming payload lease alive throughout handling.
                    let crate::Incoming { message, _bytes: _retention, .. } = incoming;
                    match message {
                        Message::Request { id, method, params } => {
                            let result = dispatch(backend.as_ref(), port.clone(), &initialized, &method, params).await.map_err(Failure::value);
                            port.respond(&id, result.as_ref()).await
                        }
                        Message::Notification { method, params } if method == "session/cancel" => {
                            if initialized.load(Ordering::Acquire) != 2 { return Err(Error::Protocol); }
                            rsi_acp_protocol::validate_session_id(&params).map_err(|_| Error::Protocol)?;
                            let request = parse(params).map_err(|_| Error::Protocol)?;
                            tokio::time::timeout(Duration::from_secs(30), backend.cancel(request)).await
                                .map_err(|_| Error::Closed)?.map_err(|_| Error::Protocol)
                        }
                        Message::Notification { .. } => Ok(()),
                        Message::Response { .. } => Err(Error::Protocol),
                    }
                });
                match std::future::poll_fn(|cx| Poll::Ready(handling.as_mut().poll(cx))).await {
                    Poll::Ready(Ok(())) => {},
                    Poll::Ready(Err(_)) => { healthy = false; break; },
                    Poll::Pending => { tasks.spawn(handling); },
                }
            }
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    let cleanup = tokio::time::timeout(Duration::from_secs(30), backend.shutdown()).await;
    healthy &= peer.handle().failure().is_none();
    healthy &= peer.close().await.is_ok();
    if healthy && matches!(cleanup, Ok(Ok(()))) {
        Ok(())
    } else {
        Err(Error::Closed)
    }
}
