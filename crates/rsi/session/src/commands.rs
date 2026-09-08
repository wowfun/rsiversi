//! Native draft command ownership and delegation to the Kernel's durable command seam.

use super::{
    HandleState, LocalSessionHandle, Result, SessionError, SessionId, TurnError, map_turn_error,
};
use futures_util::FutureExt as _;
use rsi_agent_composition_protocol::{DraftCommandError, DraftCommandPreparation};
use rsi_agent_session_protocol::{
    CommandRevision, DomainRequestId, SessionCommandInvocation, SessionCommandReceipt,
    SessionCommandsView,
};
use rsi_session_protocol::{SelectDraftPreset, SessionDraftView};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

type CommandResult = Result<SessionCommandReceipt>;
type Key = (SessionId, DomainRequestId);

pub(super) struct DraftCommands {
    pending: std::sync::Mutex<BTreeMap<Key, Pending>>,
    stopped: CancellationToken,
    tasks: TaskTracker,
}

struct Pending {
    digest: String,
    receiver: watch::Receiver<Option<CommandResult>>,
}

impl DraftCommands {
    pub(super) async fn select_preset(
        self: &Arc<Self>,
        handle: LocalSessionHandle,
        request: SelectDraftPreset,
    ) -> Result<SessionDraftView> {
        let activity = handle.begin_activity()?;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        {
            let _admission = self
                .pending
                .lock()
                .expect("draft mutation admission poisoned");
            if self.stopped.is_cancelled() {
                return Err(SessionError::ShuttingDown);
            }
            if self.tasks.len() >= 64 {
                return Err(SessionError::Capacity);
            }
            let cancellation = self.stopped.child_token();
            self.tasks.spawn(async move {
                let _activity = activity;
                let call = std::panic::AssertUnwindSafe(handle.run_preset_selection(request)).catch_unwind();
                let result = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => Err(SessionError::ShuttingDown),
                    result = tokio::time::timeout(std::time::Duration::from_secs(30), call) => match result {
                        Ok(Ok(result)) => result,
                        Ok(Err(_)) => Err(SessionError::Backend("preset preparation panicked".into())),
                        Err(_) => Err(SessionError::Invalid("preset preparation exceeded its deadline".into())),
                    }
                };
                let _ = sender.send(result);
            });
        }
        receiver
            .await
            .map_err(|_| SessionError::Backend("preset result owner disappeared".into()))?
    }
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            pending: std::sync::Mutex::new(BTreeMap::new()),
            stopped: CancellationToken::new(),
            tasks: TaskTracker::new(),
        })
    }
    pub(super) async fn stop(&self) {
        {
            let _admission = self
                .pending
                .lock()
                .expect("draft command admission poisoned");
            self.stopped.cancel();
            self.tasks.close();
        }
        self.tasks.wait().await;
    }
    async fn execute(
        self: &Arc<Self>,
        handle: LocalSessionHandle,
        invocation: SessionCommandInvocation,
    ) -> CommandResult {
        let activity = handle.begin_activity()?;
        let key = (handle.session_id.clone(), invocation.request_id.clone());
        let digest = invocation
            .digest()
            .map_err(|error| protocol_error(&error))?;
        let mut receiver = {
            let mut pending = self
                .pending
                .lock()
                .expect("draft command admission poisoned");
            if self.stopped.is_cancelled() {
                return Err(SessionError::ShuttingDown);
            }
            if let Some(existing) = pending.get(&key) {
                if existing.digest != digest {
                    return Err(SessionError::CommandConflict {
                        request_id: invocation.request_id,
                    });
                }
                existing.receiver.clone()
            } else {
                if self.tasks.len() >= 64 {
                    return Err(SessionError::Capacity);
                }
                let (sender, receiver) = watch::channel(None);
                pending.insert(
                    key.clone(),
                    Pending {
                        digest,
                        receiver: receiver.clone(),
                    },
                );
                let owner = self.clone();
                self.tasks.spawn(async move {
                    let _activity = activity;
                    let guard = PendingGuard { owner, key, sender };
                    let cancellation = guard.owner.stopped.child_token();
                    let _cancel = cancellation.clone().drop_guard();
                    let call = std::panic::AssertUnwindSafe(handle.run_draft_command(invocation, cancellation.clone())).catch_unwind();
                    let result = tokio::select! {
                        biased;
                        () = cancellation.cancelled() => Err(SessionError::ShuttingDown),
                        result = tokio::time::timeout(std::time::Duration::from_secs(30), call) => {
                            match result {
                                Ok(Ok(result)) => result,
                                Ok(Err(_)) => Err(SessionError::Backend("draft command callback panicked".into())),
                                Err(_) => Err(SessionError::Invalid("draft command exceeded its deadline".into())),
                            }
                        }
                    };
                    guard.sender.send_replace(Some(result));
                    drop(guard);
                });
                receiver
            }
        };
        loop {
            if let Some(result) = receiver.borrow_and_update().clone() {
                return result;
            }
            receiver.changed().await.map_err(|_| {
                SessionError::Backend("draft command result owner disappeared".into())
            })?;
        }
    }
}

struct PendingGuard {
    owner: Arc<DraftCommands>,
    key: Key,
    sender: watch::Sender<Option<CommandResult>>,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.owner
            .pending
            .lock()
            .expect("draft command admission poisoned")
            .remove(&self.key);
    }
}

impl LocalSessionHandle {
    pub(super) async fn read_draft_snapshot(&self) -> Result<SessionDraftView> {
        let _activity = self.begin_activity()?;
        self.reconcile_fresh_read().await?;
        let state = self.state.lock().await;
        draft_view(&state)
    }

    async fn run_preset_selection(&self, request: SelectDraftPreset) -> Result<SessionDraftView> {
        self.reconcile_fresh_read().await?;
        let preparation = {
            let state = self.state.lock().await;
            let HandleState::Fresh(draft) = &*state else {
                return Err(SessionError::NotFound("unpublished draft".into()));
            };
            let expected = CommandRevision::Draft {
                revision: request.expected_revision,
            };
            if draft.revision() != expected {
                return Err(SessionError::CommandRevisionConflict {
                    expected,
                    actual: draft.revision(),
                });
            }
            draft.prepare_preset_selection(request.preset_id)
        };
        let prepared = preparation
            .await
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        self.reconcile_fresh_read().await?;
        let mut state = self.state.lock().await;
        let HandleState::Fresh(draft) = &mut *state else {
            return Err(SessionError::NotFound("unpublished draft".into()));
        };
        draft
            .apply_preset_selection(prepared)
            .map_err(draft_error)?;
        draft_view(&state)
    }
    pub(super) async fn list_commands(&self) -> Result<SessionCommandsView> {
        let _activity = self.begin_activity()?;
        self.reconcile_fresh_read().await?;
        let state = self.state.lock().await;
        if let HandleState::Fresh(draft) = &*state {
            return SessionCommandsView::new(draft.revision(), draft.command_descriptors())
                .map_err(|error| protocol_error(&error));
        }
        state.header()?;
        drop(state);
        self.commands
            .list(
                self.turns
                    .prepare_resume(self.session_id())
                    .await
                    .map_err(map_turn_error)?,
            )
            .await
            .map_err(map_turn_error)
    }

    pub(super) async fn dispatch_command(
        &self,
        invocation: SessionCommandInvocation,
    ) -> CommandResult {
        let _activity = self.begin_activity()?;
        self.reconcile_fresh_read().await?;
        let state = self.state.lock().await;
        if let HandleState::Fresh(draft) = &*state {
            if let Some(receipt) = draft.command_receipt(&invocation.request_id) {
                if receipt.invocation_sha256()
                    != invocation
                        .digest()
                        .map_err(|error| protocol_error(&error))?
                {
                    return Err(SessionError::CommandConflict {
                        request_id: invocation.request_id,
                    });
                }
                return Ok(receipt);
            }
            drop(state);
            return self.draft_commands.execute(self.clone(), invocation).await;
        }
        state.header()?;
        drop(state);
        let request_id = invocation.request_id.clone();
        let session = self
            .turns
            .prepare_resume(self.session_id())
            .await
            .map_err(map_turn_error)?;
        let receipt = self
            .commands
            .execute(session, invocation)
            .await
            .map_err(|error| command_error(error, &request_id))?;
        SessionCommandReceipt::committed(receipt.control_seq(), receipt.commit())
            .map_err(|error| protocol_error(&error))
    }

    pub(super) async fn lookup_command(
        &self,
        request_id: &DomainRequestId,
    ) -> Result<Option<SessionCommandReceipt>> {
        let _activity = self.begin_activity()?;
        self.reconcile_fresh_read().await?;
        let state = self.state.lock().await;
        if let HandleState::Fresh(draft) = &*state {
            return Ok(draft.command_receipt(request_id));
        }
        state.header()?;
        drop(state);
        self.commands
            .query(self.session_id(), request_id)
            .await
            .map_err(|error| command_error(error, request_id))?
            .map(|receipt| {
                SessionCommandReceipt::committed(receipt.control_seq(), receipt.commit())
                    .map_err(|error| protocol_error(&error))
            })
            .transpose()
    }

    async fn run_draft_command(
        &self,
        invocation: SessionCommandInvocation,
        cancellation: CancellationToken,
    ) -> CommandResult {
        let _activity = self.begin_activity()?;
        self.reconcile_fresh_read().await?;
        let prepared = {
            let state = self.state.lock().await;
            let HandleState::Fresh(draft) = &*state else {
                return Err(SessionError::NotFound("unpublished draft".into()));
            };
            draft.prepare_command(invocation).map_err(draft_error)?
        };
        let DraftCommandPreparation::Run(prepared) = prepared else {
            let DraftCommandPreparation::Completed(receipt) = prepared else {
                unreachable!()
            };
            return Ok(receipt);
        };
        let mutation = prepared.execute(cancellation).await.map_err(draft_error)?;
        self.reconcile_fresh_read().await?;
        let mut state = self.state.lock().await;
        let HandleState::Fresh(draft) = &mut *state else {
            return Err(SessionError::NotFound("unpublished draft".into()));
        };
        draft.apply_command(mutation).map_err(draft_error)
    }
}

fn protocol_error(error: &rsi_agent_session_protocol::SessionError) -> SessionError {
    SessionError::Invalid(error.to_string())
}

fn draft_view(state: &HandleState) -> Result<SessionDraftView> {
    let HandleState::Fresh(draft) = state else {
        return Err(SessionError::NotFound("unpublished draft".into()));
    };
    let CommandRevision::Draft { revision } = draft.revision() else {
        unreachable!("draft owns a draft revision")
    };
    Ok(SessionDraftView {
        header: draft.header().clone(),
        revision,
    })
}

fn command_error(error: TurnError, request_id: &DomainRequestId) -> SessionError {
    match error {
        TurnError::DomainRequestConflict { .. } => SessionError::CommandConflict {
            request_id: request_id.clone(),
        },
        TurnError::DomainOutcomeUnknown { .. } => SessionError::CommandOutcomeUnknown {
            request_id: request_id.clone(),
        },
        TurnError::CommandRevisionConflict { expected, actual } => {
            SessionError::CommandRevisionConflict { expected, actual }
        }
        other => map_turn_error(other),
    }
}

fn draft_error(error: DraftCommandError) -> SessionError {
    match error {
        DraftCommandError::Revision { expected, actual } => {
            SessionError::CommandRevisionConflict { expected, actual }
        }
        DraftCommandError::RequestConflict { request_id } => {
            SessionError::CommandConflict { request_id }
        }
        DraftCommandError::Capacity => SessionError::Capacity,
        other => {
            let mut message = other.to_string();
            let mut end = message.len().min(4096);
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
            SessionError::Invalid(message)
        }
    }
}
