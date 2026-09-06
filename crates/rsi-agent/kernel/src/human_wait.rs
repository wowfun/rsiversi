//! Retained human/Agent waits own durable control and lane cleanup through failures.

use super::*;
use rsi_agent_session_protocol::WaitKind;
use rsi_agent_turn_protocol::HumanWait;
use rsi_tools_protocol::{ParkedToolLane, ToolLaneParkingAuthority};

pub(super) enum WaitControlError {
    Retry(TurnError),
    Permanent(TurnError),
}

impl WaitControlError {
    pub(super) fn into_turn(self) -> TurnError {
        match self {
            Self::Retry(error) | Self::Permanent(error) => error,
        }
    }
}

impl From<StoreError> for WaitControlError {
    fn from(error: StoreError) -> Self {
        match error {
            error @ (StoreError::Io(_)
            | StoreError::Conflict { .. }
            | StoreError::ControlConflict { .. }) => Self::Retry(turn_store_error(error)),
            error => Self::Permanent(turn_store_error(error)),
        }
    }
}

impl From<TurnError> for WaitControlError {
    fn from(error: TurnError) -> Self {
        match error {
            error @ (TurnError::Capacity | TurnError::Flush(_)) => Self::Retry(error),
            error => Self::Permanent(error),
        }
    }
}

struct Parked {
    kernel: SessionKernel,
    mutation: super::mutation::WaitMutationLease,
    caller: AgentCallerAuthority,
    elapsed: Option<Arc<elapsed::ElapsedState>>,
    tree_lane: Arc<TreeClaimLane>,
    executor: Option<ParkedToolLane>,
    released_tree: bool,
    boundary: Option<(
        rsi_agent_session_protocol::ActivationId,
        rsi_agent_session_protocol::StepId,
    )>,
    turn_cancellation: CancellationToken,
}

pub(super) struct Guard {
    request: Option<tokio::sync::oneshot::Sender<(WaitResumeCause, CancellationToken)>>,
    completion: tokio::task::JoinHandle<TurnResult<()>>,
}

impl std::fmt::Debug for Guard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HumanWait(..)")
    }
}

#[async_trait]
impl HumanWait for Guard {
    async fn resume(self: Box<Self>, cancellation: CancellationToken) -> TurnResult<()> {
        self.finish(WaitResumeCause::HumanAnswer, cancellation)
            .await
    }
}

impl Guard {
    pub(super) async fn finish(
        mut self: Box<Self>,
        cause: WaitResumeCause,
        cancellation: CancellationToken,
    ) -> TurnResult<()> {
        let cancellation = cancellation.child_token();
        let _cancel_on_drop = cancellation.clone().drop_guard();
        if let Some(request) = self.request.take() {
            let _ = request.send((cause, cancellation));
        }
        tokio::time::timeout(DURABILITY_WAIT_TIMEOUT, &mut self.completion)
            .await
            .map_err(|_| TurnError::Flush("wait resume did not become durable before its deadline; owned cleanup continues".into()))?
            .map_err(|error| TurnError::Invariant(format!("wait cleanup failed: {error}")))?
    }
}

impl SessionKernel {
    pub(super) async fn park_human(
        &self,
        claim: &TurnClaim,
        executor: ToolLaneParkingAuthority,
    ) -> TurnResult<Box<dyn HumanWait>> {
        let caller = self.agent_caller(claim)?;
        self.park_wait(caller, WaitKind::HumanInteraction, None, Some(executor))
            .await
            .map(|guard| guard as Box<dyn HumanWait>)
    }

    pub(super) async fn park_agent_wait(
        &self,
        caller: &AgentCallerAuthority,
        deadline_ms: u64,
    ) -> TurnResult<Box<Guard>> {
        self.park_wait(caller.clone(), WaitKind::Agent, Some(deadline_ms), None)
            .await
    }

    async fn park_wait(
        &self,
        caller: AgentCallerAuthority,
        kind: WaitKind,
        deadline_ms: Option<u64>,
        executor: Option<ToolLaneParkingAuthority>,
    ) -> TurnResult<Box<Guard>> {
        let kernel = self.clone();
        self.owned_commit(async move {
            let mutation = kernel.admit_wait_mutation(&caller)?;
            let (elapsed, tree_lane, turn_cancellation, boundary) = {
                let state = lock_state(&kernel.inner);
                let turn = kernel.validate_claim(&state, caller.claim())?;
                if turn.terminal.is_some() {
                    return Err(TurnError::StaleClaim);
                }
                if turn.cancel_requested {
                    return Err(TurnError::Cancelled);
                }
                let boundary = turn.activation_id.clone().zip(turn.current_step.clone());
                if kind == WaitKind::Agent && boundary.is_none() {
                    return Err(TurnError::Invalid(
                        "Agent wait requires an activation-owned Turn with an open Step".into(),
                    ));
                }
                let elapsed = if kind == WaitKind::HumanInteraction {
                    turn.elapsed.pause(
                        turn.accepted_at_ms,
                        kernel.inner.clock.now_ms(),
                        caller
                            .header()
                            .settings()
                            .turn_budget()
                            .maximum_elapsed_ms(),
                    )?;
                    Some(turn.elapsed.clone())
                } else {
                    None
                };
                (
                    elapsed,
                    turn.claim
                        .as_ref()
                        .expect("validated claim")
                        .tree_lane
                        .clone(),
                    turn.cancellation.clone(),
                    boundary,
                )
            };
            let mut parked = Parked {
                kernel: kernel.clone(),
                mutation,
                caller,
                elapsed,
                tree_lane,
                executor: None,
                released_tree: false,
                boundary: None,
                turn_cancellation,
            };
            let parking = async {
                if let Some((activation_id, step_id)) = boundary {
                    // Store failure may mean a committed park with a lost acknowledgement.
                    // Cleanup reconciles Running/Parked under the retained admission.
                    parked.boundary = Some((activation_id.clone(), step_id.clone()));
                    turn_service::append_retained_wait_control(
                        &kernel,
                        &parked.caller,
                        &parked.mutation,
                        &activation_id,
                        AgentControlRecordBody::WaitParked {
                            activation_id: activation_id.clone(),
                            turn_id: parked.caller.turn_id().clone(),
                            step_id: step_id.clone(),
                            kind,
                            deadline_ms,
                        },
                    )
                    .await
                    .map_err(WaitControlError::into_turn)?;
                }
                if let Some(executor) = executor {
                    parked.executor = Some(executor.park().await.map_err(lane_error)?);
                }
                parked
                    .tree_lane
                    .permit
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                parked.released_tree = true;
                kernel.request_ready_scan();
                Ok(())
            }
            .await;
            if let Err(error) = parking {
                return Err(parked.cleanup_after_failure(error).await);
            }
            Ok(Box::new(parked.track()))
        })
        .await
    }
}

impl Parked {
    async fn cleanup_after_failure(self, error: TurnError) -> TurnError {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let cleanup = Box::new(self.track())
            .finish(WaitResumeCause::Cancel, cancellation)
            .await;
        if let Err(cleanup) = cleanup
            && cleanup != TurnError::Cancelled
        {
            TurnError::Flush(bounded_diagnostic(&format!(
                "parking failed: {error}; cleanup failed: {cleanup}"
            )))
        } else {
            error
        }
    }

    fn track(self) -> Guard {
        let tasks = self.kernel.inner.tasks.clone();
        let (request, receive) = tokio::sync::oneshot::channel();
        // Registered before the admitting task finishes; the returned handle
        // only sends/closes a channel, even when dropped outside Tokio.
        let completion = tasks.spawn(async move {
            let stopping = self.mutation.stopping();
            let (cause, cancellation) = tokio::select! {
                biased;
                () = self.kernel.inner.submission_admission.closed.cancelled() => None,
                () = self.turn_cancellation.cancelled() => None,
                () = stopping.cancelled() => None,
                request = receive => request.ok(),
            }
            .unwrap_or_else(|| {
                let cancellation = CancellationToken::new();
                cancellation.cancel();
                (WaitResumeCause::Cancel, cancellation)
            });
            self.resume(cause, cancellation).await
        });
        Guard {
            request: Some(request),
            completion,
        }
    }

    async fn resume(
        mut self,
        cause: WaitResumeCause,
        cancellation: CancellationToken,
    ) -> TurnResult<()> {
        let stopping = self.mutation.stopping();
        let mut outcome = self
            .interruption(&cancellation, &stopping)
            .map_or(Ok(()), Err);
        let mut permit = None;
        // Initial claims also acquire tree admission before executor admission.
        if self.released_tree {
            permit = tokio::select! {
                biased;
                () = cancellation.cancelled() => { outcome = Err(TurnError::Cancelled); None },
                () = stopping.cancelled() => { outcome = Err(TurnError::Cancelled); None },
                () = self.turn_cancellation.cancelled() => { outcome = Err(TurnError::Cancelled); None },
                () = self.kernel.inner.submission_admission.closed.cancelled() => { outcome = Err(TurnError::ShuttingDown); None },
                permit = self.tree_lane.pool.clone().acquire_owned() => if let Ok(permit) = permit { Some(permit) } else { outcome = Err(TurnError::ShuttingDown); None },
            };
        }
        if let Some(executor) = self.executor.take() {
            let resume_cancellation = cancellation.child_token();
            if outcome.is_err() {
                resume_cancellation.cancel();
            }
            let resuming = executor.resume(resume_cancellation.clone());
            tokio::pin!(resuming);
            let result = tokio::select! {
                biased;
                () = stopping.cancelled() => { resume_cancellation.cancel(); resuming.await },
                () = self.turn_cancellation.cancelled() => { resume_cancellation.cancel(); resuming.await },
                () = self.kernel.inner.submission_admission.closed.cancelled() => { resume_cancellation.cancel(); resuming.await },
                result = &mut resuming => result,
            };
            if let Err(error) = result {
                outcome = Err(lane_error(error));
            }
        }
        loop {
            if let Some(error) = lock_state(&self.kernel.inner)
                .sessions
                .get(self.caller.session_id())
                .and_then(|session| session.permanent_flush_error.clone())
            {
                return Err(TurnError::Flush(error));
            }
            if let Some(error) = self.interruption(&cancellation, &stopping) {
                outcome = Err(error);
            }
            if let Some((activation_id, step_id)) = &self.boundary {
                let result = turn_service::append_retained_wait_control(
                    &self.kernel,
                    &self.caller,
                    &self.mutation,
                    activation_id,
                    AgentControlRecordBody::WaitResumed {
                        activation_id: activation_id.clone(),
                        turn_id: self.caller.turn_id().clone(),
                        step_id: step_id.clone(),
                        cause: if outcome.is_ok() {
                            cause
                        } else {
                            WaitResumeCause::Cancel
                        },
                    },
                )
                .await;
                match result {
                    Ok(()) => {}
                    Err(WaitControlError::Retry(_)) => {
                        // A cancelled waiter cannot retire a recoverable durable park.
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                    Err(WaitControlError::Permanent(error)) => {
                        self.mutation.fail_session(&error);
                        return Err(error);
                    }
                }
            }
            break;
        }
        if let Some(error) = self.interruption(&cancellation, &stopping) {
            outcome = Err(error);
        }
        if outcome.is_ok() && self.released_tree {
            *self
                .tree_lane
                .permit
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = permit;
        }
        outcome
    }

    fn interruption(
        &self,
        cancellation: &CancellationToken,
        stopping: &CancellationToken,
    ) -> Option<TurnError> {
        if cancellation.is_cancelled()
            || self.turn_cancellation.is_cancelled()
            || stopping.is_cancelled()
        {
            Some(TurnError::Cancelled)
        } else if self.kernel.inner.submission_admission.closed.is_cancelled() {
            Some(TurnError::ShuttingDown)
        } else {
            None
        }
    }
}

impl Drop for Parked {
    fn drop(&mut self) {
        if let Some(elapsed) = &self.elapsed {
            elapsed.resume(self.kernel.inner.clock.now_ms());
        }
    }
}

fn lane_error(error: rsi_tools_protocol::ToolError) -> TurnError {
    match error {
        rsi_tools_protocol::ToolError::Cancelled => TurnError::Cancelled,
        rsi_tools_protocol::ToolError::ShuttingDown => TurnError::ShuttingDown,
        other => TurnError::Invalid(other.to_string()),
    }
}
