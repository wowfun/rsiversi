use crate::{CONTROL, Error, Result, Setup, State, Transition, incoming};
use rsi_acp_journal::{Completion, RecordKind, Snapshot, Status};
use rsi_acp_protocol::schema;
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub(super) async fn request(
    state: &State,
    method: &str,
    params: &Value,
    timeout: Duration,
) -> Result<Value> {
    let response = tokio::time::timeout(
        timeout,
        state.port.request(method, params, state.stop.clone()),
    )
    .await
    .map_err(|_| Error::Unknown)?
    .map_err(|_| Error::Unknown)?;
    state.barrier(response.preceding_messages()).await?;
    let result = response.result().map_err(|_| Error::Remote)?;
    if method.starts_with("session/") {
        rsi_acp_protocol::validate_session_result(method, result).map_err(|_| Error::Input)?;
    }
    Ok(result.clone())
}

pub(super) async fn initialize(
    state: Arc<State>,
    mode: Setup,
    servers: Vec<schema::McpServer>,
    selections: &[rsi_acp_protocol::configuration::ConfigSelection],
) -> Result<Snapshot> {
    rsi_acp_protocol::configuration::validate(selections).map_err(|_| Error::Input)?;
    let selections = selections.to_vec();
    let operation = state
        .operation
        .clone()
        .try_lock_owned()
        .map_err(|_| Error::Busy)?;
    let task = {
        let _admission = state.permissions.lock().expect("ACP permissions");
        state.accepting()?;
        if state.initialized.load(Ordering::Acquire) {
            return Err(Error::Busy);
        }
        let owner = state.clone();
        state.tasks.spawn(async move {
            let _operation = operation;
            setup(&owner, mode, servers, &selections).await
        })
    };
    task.await.map_err(|_| Error::Unknown)?
}

async fn setup(
    state: &Arc<State>,
    mode: Setup,
    servers: Vec<schema::McpServer>,
    selections: &[rsi_acp_protocol::configuration::ConfigSelection],
) -> Result<Snapshot> {
    let mut replay_started = false;
    let result = async {
        let initialized = request(state, "initialize", &json!({"protocolVersion":1,"clientInfo":{"name":"rsiversi","version":env!("CARGO_PKG_VERSION")},"clientCapabilities":{}}), Duration::from_secs(10)).await?;
        let capabilities = rsi_acp_protocol::validate_agent_initialize(&initialized).map_err(|_| Error::Input)?;
        let saved = state.snapshot();
        let mut params = json!({"cwd":saved.cwd,"mcpServers":servers});
        let method = match mode {
            Setup::New if saved.remote.is_none() => "session/new",
            Setup::Resume if capabilities.resume && saved.remote.is_some() => "session/resume",
            Setup::Load if capabilities.load && saved.remote.is_some() => "session/load",
            _ => return Err(Error::Unsupported),
        };
        if mode != Setup::New { params["sessionId"] = json!(saved.remote); }
        rsi_acp_protocol::validate_session_setup(&params, mode != Setup::New).map_err(|_| Error::Input)?;
        if mode == Setup::Load {
            replay_started = true;
            state.transition(Transition::BeginReplay).await?;
        }
        let response = request(state, method, &params, CONTROL).await?;
        let target = if mode == Setup::New { response.get("sessionId").and_then(Value::as_str).ok_or(Error::Input)?.to_owned() } else { saved.remote.ok_or(Error::Input)? };
        state.bind_target(&target)?;
        tokio::time::timeout(CONTROL, crate::configuration::apply(state, &response, selections)).await.map_err(|_| Error::Unknown)??;
        let bound = state.transition(Transition::Bind(target, capabilities)).await?;
        replay_started = false;
        Ok(bound)
    }.await;
    if let Err(error) = result {
        if replay_started {
            let _rollback = state.transition(Transition::FinishReplay(false)).await;
        }
        let _recorded = state
            .status(if error == Error::Remote || error == Error::Unsupported {
                Status::Failed
            } else {
                Status::Unknown
            })
            .await;
        state.stop.cancel();
    }
    result
}

pub(super) async fn submit(
    state: Arc<State>,
    prompt: Vec<schema::ContentBlock>,
) -> Result<Snapshot> {
    state.accepting()?;
    if !state.initialized.load(Ordering::Acquire) {
        return Err(Error::Busy);
    }
    let operation = state
        .operation
        .clone()
        .try_lock_owned()
        .map_err(|_| Error::Busy)?;
    let parameters = json!({"sessionId":state.target()?,"prompt":prompt});
    rsi_acp_protocol::validate_prompt(&parameters).map_err(|_| Error::Input)?;
    let (accepted, receive) = tokio::sync::oneshot::channel();
    // Closing admission and task registration share the permission lock.
    {
        let _admission = state.permissions.lock().expect("ACP permissions");
        state.accepting()?;
        let cancellation = CancellationToken::new();
        *state.active.lock().expect("ACP active prompt") = Some(cancellation.clone());
        let owner = state.clone();
        state.tasks.spawn(async move {
            let result = run_prompt(&owner, &parameters, &cancellation, accepted).await;
            if let Err(error) = result {
                let _recorded = owner
                    .status(if error == Error::Remote {
                        Status::Failed
                    } else {
                        Status::Unknown
                    })
                    .await;
                if error != Error::Remote {
                    owner.stop.cancel();
                }
            }
            incoming::close_permission_admission(&owner);
            drop(operation);
        });
    }
    receive.await.map_err(|_| Error::Journal)?
}

async fn run_prompt(
    state: &Arc<State>,
    parameters: &Value,
    cancellation: &CancellationToken,
    accepted: tokio::sync::oneshot::Sender<Result<Snapshot>>,
) -> Result<()> {
    state
        .journal
        .append(
            &state.id,
            state.generation,
            RecordKind::User,
            parameters["prompt"].clone(),
        )
        .await
        .map_err(|_| Error::Journal)?;
    let snapshot = state.status(Status::Running).await?;
    let _accepted = accepted.send(Ok(snapshot));
    if cancellation.is_cancelled() {
        state.status(Status::Discarded).await?;
        return Ok(());
    }
    // First-poll request admission precedes cancel's write on the same port.
    let response = state
        .port
        .request("session/prompt", parameters, state.stop.clone());
    tokio::pin!(response);
    let response = tokio::select! { biased;
        result = &mut response => result.map_err(|_| Error::Unknown)?,
        () = cancellation.cancelled() => {
            tokio::time::timeout(CONTROL, async {
                let cleanup = async {
                    state.port.notify("session/cancel", &json!({"sessionId":state.target()?})).await.map_err(|_| Error::Unknown)?;
                    incoming::cancel_permissions(state).await
                };
                // A final response is authoritative even while cancellation writes
                // are blocked or fail. Keep polling its exact correlation waiter.
                tokio::select! { biased;
                    result = &mut response => result,
                    _ = cleanup => response.await,
                }.map_err(|_| Error::Unknown)
            }).await.map_err(|_| Error::Unknown)??
        }
    };
    state.barrier(response.preceding_messages()).await?;
    let result = response.result().map_err(|_| Error::Remote)?;
    rsi_acp_protocol::validate_session_result("session/prompt", result)
        .map_err(|_| Error::Unknown)?;
    let completion = match result.get("stopReason").and_then(Value::as_str) {
        Some("end_turn") => Completion::EndTurn,
        Some("max_tokens") => Completion::MaxTokens,
        Some("max_turn_requests") => Completion::MaxTurnRequests,
        Some("refusal") => Completion::Refusal,
        Some("cancelled") => Completion::Cancelled,
        _ => return Err(Error::Unknown),
    };
    incoming::close_permission_admission(state);
    let completed = state.transition(Transition::Complete(completion)).await;
    let cancelled = incoming::cancel_permissions(state).await;
    completed.and(cancelled)
}

pub(super) async fn cancel(state: &Arc<State>) -> Result<()> {
    if state.operation.try_lock().is_ok() {
        return Ok(());
    }
    let cancellation = state
        .active
        .lock()
        .expect("ACP active prompt")
        .clone()
        .ok_or(Error::Busy)?;
    cancellation.cancel();
    match tokio::time::timeout(CONTROL, state.operation.lock()).await {
        Ok(_settled)
            if !state.stop.is_cancelled()
                && !matches!(
                    state.snapshot().status,
                    Status::Running | Status::Loading | Status::Starting | Status::Unknown
                ) =>
        {
            Ok(())
        }
        _ => {
            let _recorded = state.status(Status::Unknown).await;
            state.stop.cancel();
            Err(Error::Unknown)
        }
    }
}
