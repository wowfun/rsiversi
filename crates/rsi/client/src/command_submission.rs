use crate::{SessionController, command_invocation};
use rsi_agent_session_protocol::{
    CommandArguments, DomainRequestId, SessionCommandInvocation, SessionCommandReceipt,
};
use rsi_session_protocol::{Result, SessionError};
use std::sync::{Arc, Mutex};

/// One saved Session's bounded command draft; applications retain it across surface replacement.
#[derive(Debug)]
pub struct CommandSubmission {
    view: Mutex<CommandSubmissionView>,
    work: tokio::sync::Semaphore,
}
/// Presentation snapshot retaining one original invocation or its last compact receipt.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct CommandSubmissionView {
    /// Unresolved exact input; refreshing it can only query its receipt.
    pub pending: Option<SessionCommandInvocation>,
    /// Most recent successfully reconciled receipt.
    pub receipt: Option<SessionCommandReceipt>,
}
impl Default for CommandSubmission {
    fn default() -> Self {
        Self {
            view: Mutex::default(),
            work: tokio::sync::Semaphore::new(1),
        }
    }
}
impl CommandSubmission {
    /// Dispatches registered slash names; other text remains available to Human-message parsing.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned the saved command state.
    pub async fn try_slash(
        &self,
        controller: &Arc<SessionController>,
        text: &str,
        request_id: DomainRequestId,
    ) -> Result<Option<SessionCommandReceipt>> {
        let Some((name, arguments)) = crate::slash_command(text) else {
            return Ok(None);
        };
        let _work = self
            .work
            .try_acquire()
            .map_err(|_| SessionError::Capacity)?;
        if let Some(pending) = self.view().pending {
            return Err(SessionError::CommandOutcomeUnknown {
                request_id: pending.request_id,
            });
        }
        let view = controller.commands().await?;
        if !view.commands().iter().any(|entry| entry.name() == name) {
            return Ok(None);
        }
        let arguments = CommandArguments::new(arguments.into())
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        let invocation = command_invocation(&view, name, arguments, request_id)?;
        self.view
            .lock()
            .expect("command submission poisoned")
            .pending = Some(invocation.clone());
        let result = controller.execute_command(invocation).await;
        self.record(&result);
        result.map(Some)
    }
    /// Copies one bounded saved invocation and compact receipt for presentation.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned the saved command state.
    pub fn view(&self) -> CommandSubmissionView {
        self.view
            .lock()
            .expect("command submission poisoned")
            .clone()
    }

    /// Discovers and freezes a new explicit invocation before sending it once.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned the saved command state.
    pub async fn execute(
        &self,
        controller: &Arc<SessionController>,
        name: &str,
        arguments: CommandArguments,
        request_id: DomainRequestId,
    ) -> Result<SessionCommandReceipt> {
        let _work = self
            .work
            .try_acquire()
            .map_err(|_| SessionError::Capacity)?;
        if let Some(pending) = self.view().pending {
            return Err(SessionError::CommandOutcomeUnknown {
                request_id: pending.request_id,
            });
        }
        let invocation =
            command_invocation(&controller.commands().await?, name, arguments, request_id)?;
        self.view
            .lock()
            .expect("command submission poisoned")
            .pending = Some(invocation.clone());
        let result = controller.execute_command(invocation).await;
        self.record(&result);
        result
    }
    /// Queries the frozen input; absent or failed lookup leaves it retained.
    pub async fn refresh(
        &self,
        controller: &Arc<SessionController>,
    ) -> Result<SessionCommandReceipt> {
        let _work = self
            .work
            .try_acquire()
            .map_err(|_| SessionError::Capacity)?;
        let invocation = self
            .view()
            .pending
            .ok_or_else(|| SessionError::Invalid("No unresolved Session command".into()))?;
        let result = controller.reconcile_command(invocation).await;
        if result.is_ok() {
            self.record(&result);
        }
        result
    }
    fn record(&self, result: &Result<SessionCommandReceipt>) {
        let mut view = self.view.lock().expect("command submission poisoned");
        match result {
            Ok(receipt) => {
                view.pending = None;
                view.receipt = Some(receipt.clone());
            }
            Err(SessionError::CommandOutcomeUnknown { .. }) => {}
            Err(_) => {
                view.pending = None;
            }
        }
    }
}
