//! Exact Session draft/Workspace ownership behind the Host controller's narrow bridge.

use super::{HandleState, LocalSessionHandle, SessionError, TurnError};
use async_trait::async_trait;
use futures_util::StreamExt;
use rsi_agent_goal::{GOAL_DOMAIN, GoalState, RoundOutcome, RoundSettlement};
use rsi_agent_session_protocol::{
    CommandRevision, ContinuationInput, ContinuationProvenance, DomainRequestId, DomainRevision,
    DomainStateView, MessageId, SessionCommandInvocation, SessionCommandReceipt, SessionId,
};
use rsi_agent_turn_protocol::{
    CancelTarget, ContinuationBinding, ContinuationLease, DomainMutationReceipt, MessageReceipt,
    MessageState, SessionContinuations, SubmitSession,
};
use rsi_goal::{GoalError, GoalResult, GoalSession, GoalSnapshot};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

impl LocalSessionHandle {
    fn continuation_service(&self) -> GoalResult<&Arc<dyn SessionContinuations>> {
        self.continuations.as_ref().ok_or(GoalError::Unavailable)
    }

    async fn goal_snapshot(&self) -> GoalResult<GoalSnapshot> {
        let _activity = self.begin_activity().map_err(goal_error)?;
        self.reconcile_fresh_read().await.map_err(goal_error)?;
        let state = self.state.lock().await;
        let header = state.header().map_err(goal_error)?.clone();
        let (revision, domain) = if let HandleState::Fresh(draft) = &*state {
            let snapshot = draft
                .baseline()
                .initial_states()
                .into_iter()
                .find(|snapshot| snapshot.identity().id() == GOAL_DOMAIN)
                .ok_or(GoalError::Unavailable)?;
            (
                draft.revision(),
                DomainStateView {
                    revision: DomainRevision::new(0),
                    snapshot,
                },
            )
        } else {
            drop(state);
            let page = self
                .store
                .read_domain_states(self.session_id(), None)
                .await
                .map_err(backend)?;
            page.validate().map_err(backend)?;
            if page.selected_control_seq != page.durable_control_seq {
                return Err(GoalError::Backend(
                    "Goal domain read returned a historical cut".into(),
                ));
            }
            let state = page
                .states
                .into_iter()
                .find(|state| state.snapshot.identity().id() == GOAL_DOMAIN)
                .ok_or(GoalError::Unavailable)?;
            (
                CommandRevision::Durable {
                    control_seq: page.durable_control_seq,
                },
                DomainStateView {
                    revision: state.head.revision,
                    snapshot: state.snapshot,
                },
            )
        };
        let state = GoalState::decode(&domain.snapshot).map_err(GoalError::Invalid)?;
        Ok(GoalSnapshot {
            header,
            revision,
            domain,
            state,
        })
    }

    async fn retain_goal(
        &self,
        binding: ContinuationBinding,
        armed: bool,
    ) -> GoalResult<ContinuationLease> {
        let _activity = self.begin_activity().map_err(goal_error)?;
        self.reconcile_fresh_read().await.map_err(goal_error)?;
        let state = self.state.lock().await;
        let session = if let HandleState::Fresh(draft) = &*state {
            SubmitSession::Fresh(draft.freeze())
        } else {
            state.header().map_err(goal_error)?;
            SubmitSession::Resume(
                self.turns
                    .prepare_resume(self.session_id())
                    .await
                    .map_err(turn_error)?,
            )
        };
        let service = self.continuation_service()?;
        if armed {
            service.arm(session, binding).await.map_err(turn_error)
        } else {
            service
                .retain_for_settlement(session, binding)
                .await
                .map_err(turn_error)
        }
    }

    async fn goal_round_outcome(&self, message: &MessageId) -> GoalResult<Option<RoundSettlement>> {
        let receipt = self
            .turns
            .message_status(self.session_id(), message)
            .await
            .map_err(turn_error)?;
        match receipt.state {
            MessageState::Pending => Ok(None),
            MessageState::Discarded { .. } => Ok(Some(RoundSettlement::Discarded)),
            MessageState::Claimed { turn_id, .. } => {
                let outcome = self
                    .turns
                    .outcome(self.session_id(), &turn_id)
                    .await
                    .map_err(turn_error)?;
                Ok(outcome.map(|outcome| RoundSettlement::Turn {
                    turn_id,
                    outcome: RoundOutcome::from(&outcome),
                }))
            }
        }
    }
}

#[async_trait]
impl GoalSession for LocalSessionHandle {
    fn session_id(&self) -> &SessionId {
        self.session_id()
    }

    async fn snapshot(&self) -> GoalResult<GoalSnapshot> {
        deadline(self.goal_snapshot()).await
    }

    async fn application_command(
        &self,
        invocation: SessionCommandInvocation,
    ) -> GoalResult<SessionCommandReceipt> {
        self.dispatch_command(invocation).await.map_err(goal_error)
    }

    async fn command_status(
        &self,
        request: &DomainRequestId,
    ) -> GoalResult<Option<SessionCommandReceipt>> {
        rsi_session_protocol::SessionHandle::command_status(self, request)
            .await
            .map_err(goal_error)
    }

    async fn arm(&self, binding: ContinuationBinding) -> GoalResult<ContinuationLease> {
        deadline(self.retain_goal(binding, true)).await
    }
    async fn retain_for_settlement(
        &self,
        binding: ContinuationBinding,
    ) -> GoalResult<ContinuationLease> {
        deadline(self.retain_goal(binding, false)).await
    }

    async fn internal_command(
        &self,
        lease: &ContinuationLease,
        invocation: SessionCommandInvocation,
        input: Option<ContinuationInput>,
    ) -> GoalResult<DomainMutationReceipt> {
        let prepared = self
            .turns
            .prepare_resume(self.session_id())
            .await
            .map_err(turn_error)?;
        self.continuation_service()?
            .execute(lease, prepared, invocation, input)
            .await
            .map_err(turn_error)
    }

    async fn internal_status(
        &self,
        lease: &ContinuationLease,
        request: &DomainRequestId,
    ) -> GoalResult<Option<DomainMutationReceipt>> {
        self.continuation_service()?
            .query(lease, request)
            .await
            .map_err(turn_error)
    }

    async fn submit(
        &self,
        lease: &ContinuationLease,
        input: ContinuationInput,
        provenance: ContinuationProvenance,
    ) -> GoalResult<MessageReceipt> {
        let _activity = self.begin_activity().map_err(goal_error)?;
        input
            .validate()
            .map_err(|error| GoalError::Invalid(error.to_string()))?;
        let header = self.header_snapshot().await.map_err(goal_error)?;
        self.language
            .describe(header.settings().default_model())
            .map_err(|error| GoalError::Invalid(error.to_string()))?;
        let mut state = self.state.lock().await;
        if matches!(*state, HandleState::Attached(_)) {
            drop(state);
            let session = SubmitSession::Resume(
                self.turns
                    .prepare_resume(self.session_id())
                    .await
                    .map_err(turn_error)?,
            );
            self.prepare_workspace(&header).await.map_err(goal_error)?;
            return self
                .continuation_service()?
                .submit(lease, session, input, provenance)
                .await
                .map_err(turn_error);
        }
        let HandleState::Fresh(draft) = &*state else {
            return Err(GoalError::Unavailable);
        };
        let session = SubmitSession::Fresh(draft.freeze());
        self.prepare_workspace(&header).await.map_err(goal_error)?;
        let result = self
            .continuation_service()?
            .submit(lease, session, input, provenance)
            .await;
        self.reconcile_fresh_submission(&mut state, result.is_ok())
            .await;
        result.map_err(turn_error)
    }

    async fn message_status(&self, message: &MessageId) -> GoalResult<Option<MessageReceipt>> {
        match self.turns.message_status(self.session_id(), message).await {
            Ok(receipt) => Ok(Some(receipt)),
            Err(TurnError::MessageNotFound { .. } | TurnError::SessionNotFound(_)) => Ok(None),
            Err(error) => Err(turn_error(error)),
        }
    }

    async fn wait_round(
        &self,
        message: &MessageId,
        cancellation: CancellationToken,
    ) -> GoalResult<RoundSettlement> {
        let mut changes = self
            .projection_service
            .watch_projection_changes(self.session_id())
            .map_err(turn_error)?;
        loop {
            let outcome = tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(GoalError::ShuttingDown),
                outcome = deadline(self.goal_round_outcome(message)) => outcome?,
            };
            if let Some(outcome) = outcome {
                return Ok(outcome);
            }
            tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(GoalError::ShuttingDown),
                update = changes.next() => if update.is_none() { return Err(GoalError::Backend("Goal outcome observation ended".into())); },
            }
        }
    }

    async fn discard_if_pending(
        &self,
        lease: &ContinuationLease,
        message: &MessageId,
    ) -> GoalResult<Option<MessageReceipt>> {
        match self
            .continuation_service()?
            .discard_if_pending(lease, message)
            .await
        {
            Ok(receipt) => Ok(Some(receipt)),
            Err(TurnError::MessageNotFound { .. } | TurnError::SessionNotFound(_)) => Ok(None),
            Err(error) => Err(turn_error(error)),
        }
    }

    async fn cancel(&self, message: &MessageId) -> GoalResult<()> {
        self.turns
            .cancel_target(
                self.session_id(),
                CancelTarget::Message(message.clone()),
                None,
            )
            .await
            .map_err(turn_error)?;
        Ok(())
    }
}

async fn deadline<T>(future: impl std::future::Future<Output = GoalResult<T>>) -> GoalResult<T> {
    tokio::time::timeout(std::time::Duration::from_secs(30), future)
        .await
        .map_err(|_| GoalError::Backend("Goal state read or preparation deadline elapsed".into()))?
}

fn backend(error: impl std::fmt::Display) -> GoalError {
    GoalError::Backend(error.to_string())
}

fn goal_error(error: SessionError) -> GoalError {
    match error {
        SessionError::Invalid(message) => GoalError::Invalid(message),
        SessionError::NotFound(_) => GoalError::Unavailable,
        SessionError::CommandRevisionConflict { expected, actual } => {
            GoalError::RevisionConflict { expected, actual }
        }
        SessionError::CommandConflict { .. } | SessionError::MessageConflict { .. } => {
            GoalError::Conflict
        }
        SessionError::CommandOutcomeUnknown { request_id } => {
            GoalError::OutcomeUnknown(request_id.to_string())
        }
        SessionError::Capacity => GoalError::Capacity,
        SessionError::ShuttingDown => GoalError::ShuttingDown,
        other => backend(other),
    }
}

fn turn_error(error: TurnError) -> GoalError {
    match error {
        TurnError::ContinuationDisarmed => GoalError::Disarmed,
        TurnError::DomainOutcomeUnknown { request_id } => GoalError::OutcomeUnknown(request_id),
        TurnError::CommandRevisionConflict { expected, actual } => {
            GoalError::RevisionConflict { expected, actual }
        }
        TurnError::DomainRequestConflict { .. } | TurnError::MessageConflict { .. } => {
            GoalError::Conflict
        }
        TurnError::Invalid(message) => GoalError::Invalid(message),
        TurnError::Capacity | TurnError::ObserverCapacity | TurnError::ProjectionCapacity => {
            GoalError::Capacity
        }
        TurnError::ShuttingDown => GoalError::ShuttingDown,
        other => backend(other),
    }
}

pub(super) fn session_error(error: GoalError) -> SessionError {
    match error {
        GoalError::Unavailable => SessionError::NotFound("Goal controller or state".into()),
        GoalError::Capacity => SessionError::Capacity,
        GoalError::ShuttingDown => SessionError::ShuttingDown,
        GoalError::Invalid(message) => SessionError::Invalid(message),
        GoalError::RevisionConflict { expected, actual } => {
            SessionError::CommandRevisionConflict { expected, actual }
        }
        GoalError::OutcomeUnknown(request_id) => match DomainRequestId::new(request_id) {
            Ok(request_id) => SessionError::CommandOutcomeUnknown { request_id },
            Err(error) => SessionError::Backend(error.to_string()),
        },
        other => SessionError::Backend(other.to_string()),
    }
}
