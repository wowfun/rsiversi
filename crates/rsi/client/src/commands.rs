use crate::SessionController;
use futures_util::future::BoxFuture;
use rsi_agent_session_protocol::{
    CommandArguments, CommandRevision, DomainRequestId, SessionCommandInvocation,
    SessionCommandReceipt, SessionCommandsView,
};
use rsi_session_protocol::{Result, SessionError, SessionHandle};
use std::sync::Arc;

/// A slash command borrows its name and text arguments; `//` is ordinary message text.
pub fn slash_command(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_start().strip_prefix('/')?;
    if text.starts_with('/') {
        return None;
    }
    Some(
        text.split_once(char::is_whitespace)
            .map_or((text, ""), |(name, arguments)| (name, arguments.trim())),
    )
}

/// Freezes dispatch identity and predecessor from one discovery snapshot.
pub fn command_invocation(
    view: &SessionCommandsView,
    name: &str,
    arguments: CommandArguments,
    request_id: DomainRequestId,
) -> Result<SessionCommandInvocation> {
    let command = view
        .commands()
        .iter()
        .find(|command| command.name() == name)
        .ok_or_else(|| {
            SessionError::Invalid(format!(
                "Unknown Session command /{name}; refresh the command list"
            ))
        })?;
    if matches!(view.revision(), CommandRevision::Draft { .. }) && !command.draft_safe() {
        return Err(SessionError::Invalid(format!(
            "/{name} requires a published Session"
        )));
    }
    Ok(SessionCommandInvocation {
        command: command.id().clone(),
        request_id,
        expected_revision: view.revision(),
        arguments,
    })
}

fn unknown(invocation: &SessionCommandInvocation) -> SessionError {
    SessionError::CommandOutcomeUnknown {
        request_id: invocation.request_id.clone(),
    }
}

/// Sends once, then queries an uncertain result once. It never replays a callback.
pub async fn execute_command_once(
    handle: &dyn SessionHandle,
    invocation: SessionCommandInvocation,
) -> Result<SessionCommandReceipt> {
    match handle.execute_command(invocation.clone()).await {
        Ok(receipt) => matching_receipt(&invocation, receipt),
        Err(
            SessionError::CommandOutcomeUnknown { .. }
            | SessionError::Api(rsi_api_protocol::ApiError::OutcomeUnknown),
        ) => query_command_result(handle, &invocation).await,
        Err(error) => Err(error),
    }
}

/// Queries the original invocation and preserves uncertainty on absence or query failure.
pub async fn query_command_result(
    handle: &dyn SessionHandle,
    invocation: &SessionCommandInvocation,
) -> Result<SessionCommandReceipt> {
    match handle.command_status(&invocation.request_id).await {
        Ok(Some(receipt)) => matching_receipt(invocation, receipt),
        _ => Err(unknown(invocation)),
    }
}
fn matching_receipt(
    invocation: &SessionCommandInvocation,
    receipt: SessionCommandReceipt,
) -> Result<SessionCommandReceipt> {
    if receipt.command() != &invocation.command
        || receipt.request_id() != &invocation.request_id
        || invocation.digest().ok().as_deref() != Some(receipt.invocation_sha256())
    {
        return Err(unknown(invocation));
    }
    Ok(receipt)
}

impl SessionController {
    /// Reads the current pinned descriptors and predecessor under bounded read admission.
    pub async fn commands(&self) -> Result<SessionCommandsView> {
        if self.stop.is_cancelled() {
            return Err(SessionError::ShuttingDown);
        }
        let _permit = self
            .submissions
            .clone()
            .try_acquire_owned()
            .map_err(|_| SessionError::Capacity)?;
        tokio::select! { biased;
            () = self.stop.cancelled() => Err(SessionError::ShuttingDown),
            result = crate::read_with_capacity_retry(&self.execution, || self.handle.commands()) => result,
        }
    }
    /// Admits one command before returning its waiter; caller retains the frozen invocation.
    pub fn execute_command(
        self: &Arc<Self>,
        invocation: SessionCommandInvocation,
    ) -> BoxFuture<'static, Result<SessionCommandReceipt>> {
        self.command_work(invocation, false)
    }
    /// Refreshes an unresolved receipt without another mutation or a replacement identity.
    pub fn reconcile_command(
        self: &Arc<Self>,
        invocation: SessionCommandInvocation,
    ) -> BoxFuture<'static, Result<SessionCommandReceipt>> {
        self.command_work(invocation, true)
    }
    fn command_work(
        self: &Arc<Self>,
        invocation: SessionCommandInvocation,
        query: bool,
    ) -> BoxFuture<'static, Result<SessionCommandReceipt>> {
        let admission = self
            .admission
            .lock()
            .expect("controller admission poisoned");
        if self.stop.is_cancelled() {
            return Box::pin(async { Err(SessionError::ShuttingDown) });
        }
        let Ok(permit) = self.submissions.clone().try_acquire_owned() else {
            return Box::pin(async { Err(SessionError::Capacity) });
        };
        let unresolved = unknown(&invocation);
        let fallback = unresolved.clone();
        let controller = self.clone();
        let task = self.execution.spawn(self.tasks.track_future(async move {
            let _permit = permit;
            tokio::select! { biased;
                () = controller.stop.cancelled() => Err(unresolved),
                result = async {
                    if query { query_command_result(controller.handle.as_ref(), &invocation).await }
                    else { execute_command_once(controller.handle.as_ref(), invocation).await }
                } => result,
            }
        }));
        drop(admission);
        Box::pin(async move { task.await.unwrap_or(Err(fallback)) })
    }
}
