use super::{Arc, BoxFuture, ObservationCursor, Result, SessionController, SessionError};
use rsi_goal::{GoalControl, GoalControlReceipt, GoalLiveState};
use rsi_session_protocol::{JobsSnapshot, ProjectionSnapshot};

/// Latest explicitly admitted Goal control and its retained feedback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GoalControlState {
    /// No unresolved control or retained rejection.
    Idle,
    /// Original request retained until its outcome is known; never implicitly replayed.
    Pending(GoalControl),
    /// Known rejection retained across ordinary observation refreshes.
    Rejected {
        /// Identity of the rejected request.
        request_id: rsi_agent_session_protocol::DomainRequestId,
        /// UTF-8 diagnostic bounded by the Session diagnostic limit.
        diagnostic: String,
    },
}

fn rejection_diagnostic(error: &SessionError) -> String {
    let mut diagnostic = error.to_string();
    let mut length = diagnostic
        .len()
        .min(rsi_agent_session_protocol::MAXIMUM_AGENT_DIAGNOSTIC_BYTES);
    while !diagnostic.is_char_boundary(length) {
        length -= 1;
    }
    diagnostic.truncate(length);
    diagnostic
}

fn activation(value: &Option<Result<GoalLiveState>>) -> Option<bool> {
    let live = value.as_ref()?.as_ref().ok()?;
    live.available.then_some(live.armed)
}

impl SessionController {
    pub(super) fn publish_goal(&self, value: Option<Result<GoalLiveState>>) {
        let mut activation_changed = false;
        self.goal.send_if_modified(|previous| {
            if *previous == value {
                return false;
            }
            activation_changed = activation(previous) != activation(&value);
            *previous = value;
            true
        });
        if activation_changed {
            self.sink.goal_activation_changed();
        }
    }
    /// Latest observed authority to schedule Goal rounds, absent until available.
    pub fn goal_armed(&self) -> Option<bool> {
        activation(&self.goal.borrow())
    }
    /// Shares the already owned projection stream's latest retained snapshot.
    pub fn projection_changes(&self) -> tokio::sync::watch::Receiver<Option<ProjectionSnapshot>> {
        self.projections.subscribe()
    }
    /// Observes process-local driving, independently of durable Goal state.
    pub fn goal_changes(&self) -> tokio::sync::watch::Receiver<Option<Result<GoalLiveState>>> {
        self.goal.subscribe()
    }
    /// Observes exact pending identity and known rejection independently of live/projection updates.
    pub fn goal_control_changes(&self) -> tokio::sync::watch::Receiver<GoalControlState> {
        self.goal_control.subscribe()
    }

    /// Reads one current-Turn page; an absent active Turn has no process-local Jobs.
    pub async fn jobs(
        &self,
        page: Option<rsi_agent_turn_protocol::TurnJobsRequest>,
    ) -> Result<Option<JobsSnapshot>> {
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
            result = async {
                let request = if let Some(request) = page { request } else {
                        let inspection = crate::read_with_capacity_retry(&self.execution, || self.handle.inspect()).await?;
                        let Some(turn_id) = inspection.active_turn_id else { return Ok(None); };
                        rsi_agent_turn_protocol::TurnJobsRequest { turn_id, generation: None, after: None, limit: 16 }
                };
                crate::read_with_capacity_retry(&self.execution, || self.handle.read_jobs(request.clone())).await.map(Some)
            } => result,
        }
    }

    /// Admits one explicit Goal control once; unknown outcomes retain its exact identity.
    pub fn control_goal(
        self: &Arc<Self>,
        request: GoalControl,
    ) -> BoxFuture<'static, Result<GoalControlReceipt>> {
        self.goal_work(request, false)
    }
    /// Reads the pending control's receipt and current live state without arming a driver.
    pub fn reconcile_goal(self: &Arc<Self>) -> BoxFuture<'static, Result<GoalControlReceipt>> {
        let GoalControlState::Pending(request) = self.goal_control.borrow().clone() else {
            return Box::pin(async {
                Err(SessionError::Invalid("No unresolved Goal control".into()))
            });
        };
        self.goal_work(request, true)
    }
    fn goal_work(
        self: &Arc<Self>,
        request: GoalControl,
        query: bool,
    ) -> BoxFuture<'static, Result<GoalControlReceipt>> {
        let _admission = self
            .admission
            .lock()
            .expect("controller admission poisoned");
        if self.stop.is_cancelled() {
            return Box::pin(async { Err(SessionError::ShuttingDown) });
        }
        let Ok(permit) = self.submissions.clone().try_acquire_owned() else {
            return Box::pin(async { Err(SessionError::Capacity) });
        };
        if matches!(&*self.goal_control.borrow(), GoalControlState::Pending(pending) if !query || *pending != request)
        {
            return Box::pin(async {
                Err(SessionError::Invalid(
                    "Reconcile the unresolved Goal control first".into(),
                ))
            });
        }
        self.goal_control.send_if_modified(|state| {
            if matches!(state, GoalControlState::Pending(pending) if *pending == request) {
                return false;
            }
            *state = GoalControlState::Pending(request.clone());
            true
        });
        let unknown = SessionError::CommandOutcomeUnknown {
            request_id: request.request_id.clone(),
        };
        let fallback = unknown.clone();
        let controller = self.clone();
        let request_id = request.request_id.clone();
        let task = self.execution.spawn(self.tasks.track_future(async move {
            let _permit = permit;
            let result = tokio::select! { biased;
                () = controller.stop.cancelled() => Err(unknown.clone()),
                result = async {
                    if query {
                        let invocation = request.invocation().map_err(|error| SessionError::Invalid(error.to_string()))?;
                        let command = crate::query_command_result(controller.handle.as_ref(), &invocation).await?;
                        let live = controller.handle.goal_status().await.map_err(|_| unknown.clone())?;
                        Ok(GoalControlReceipt { command, live })
                    } else { controller.handle.control_goal(request).await }
                } => result,
            };
            let uncertain = matches!(&result, Err(SessionError::CommandOutcomeUnknown { .. } | SessionError::Api(rsi_api_protocol::ApiError::OutcomeUnknown)));
            controller.goal_control.send_if_modified(|state| {
                if !matches!(state, GoalControlState::Pending(request) if request.request_id == request_id) { return false; }
                if uncertain { return false; }
                *state = match &result {
                    Ok(_) => GoalControlState::Idle,
                    Err(error) => GoalControlState::Rejected { request_id, diagnostic: rejection_diagnostic(error) },
                };
                true
            });
            if result.is_ok() || uncertain { controller.start_observing(ObservationCursor::default()); }
            result
        }));
        Box::pin(async move { task.await.unwrap_or(Err(fallback)) })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn rejection_diagnostic_preserves_utf8_within_the_session_bound() {
        let error = super::SessionError::Invalid("证".repeat(4096));
        let diagnostic = super::rejection_diagnostic(&error);
        assert!(diagnostic.len() <= rsi_agent_session_protocol::MAXIMUM_AGENT_DIAGNOSTIC_BYTES);
        assert!(error.to_string().starts_with(&diagnostic));
        assert!(diagnostic.ends_with('证'));
    }
}
