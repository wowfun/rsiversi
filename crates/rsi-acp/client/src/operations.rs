use crate::{CONTROL, Error, Result, Setup, State, incoming};
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
    state: &State,
    mode: Setup,
    servers: Vec<schema::McpServer>,
    selections: &[rsi_acp_protocol::configuration::ConfigSelection],
) -> Result<Snapshot> {
    rsi_acp_protocol::configuration::validate(selections).map_err(|_| Error::Input)?;
    state.accepting()?;
    let _operation = state.operation.try_lock().map_err(|_| Error::Busy)?;
    if state.initialized.load(Ordering::Acquire) {
        return Err(Error::Busy);
    }
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
            let loading = state.journal.begin_replay(&state.id, state.generation).await.map_err(|_| Error::Journal)?;
            *state.snapshot.lock().expect("ACP client snapshot") = loading;
            state.changed();
        }
        let response = request(state, method, &params, CONTROL).await?;
        let target = if mode == Setup::New { response.get("sessionId").and_then(Value::as_str).ok_or(Error::Input)?.to_owned() } else { saved.remote.ok_or(Error::Input)? };
        state.bind_target(&target)?;
        tokio::time::timeout(CONTROL, crate::configuration::apply(state, &response, selections)).await.map_err(|_| Error::Unknown)??;
        if mode == Setup::Load { state.journal.finish_replay(&state.id, state.generation, true).await.map_err(|_| Error::Journal)?; }
        state.accepting()?;
        let snapshot = state.journal.bind(&state.id, state.generation, target, capabilities).await.map_err(|_| Error::Journal)?;
        {
            let mut current = state.snapshot.lock().expect("ACP client snapshot");
            // EOF may settle while the durable bind waits for its blocking worker.
            // Check under the same lock used by the reader's final publication.
            state.accepting()?;
            *current = snapshot.clone();
            state.initialized.store(true, Ordering::Release);
        }
        state.changed();
        Ok(snapshot)
    }.await;
    if let Err(error) = result {
        if state.snapshot().status == Status::Loading {
            let _rollback = state
                .journal
                .finish_replay(&state.id, state.generation, false)
                .await;
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
            owner.active.lock().expect("ACP active prompt").take();
            drop(operation);
        });
    }
    receive.await.map_err(|_| Error::Journal)?
}

async fn run_prompt(
    state: &State,
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
                state.port.notify("session/cancel", &json!({"sessionId":state.target()?})).await.map_err(|_| Error::Unknown)?;
                incoming::cancel_permissions(state).await?;
                response.await.map_err(|_| Error::Unknown)
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
    incoming::cancel_permissions(state).await?;
    let snapshot = state
        .journal
        .complete(&state.id, state.generation, completion)
        .await
        .map_err(|_| Error::Journal)?;
    *state.snapshot.lock().expect("ACP client snapshot") = snapshot;
    state.changed();
    Ok(())
}

pub(super) async fn cancel(state: &State) -> Result<()> {
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
