use super::{Arc, BoxFuture, ObservationCursor, Result, SessionController, SessionError};
use futures_util::StreamExt;
use rsi_goal::{GoalControl, GoalControlReceipt, GoalLiveState};
use rsi_session_protocol::{JobsSnapshot, ProjectionSnapshot};

impl SessionController {
    fn publish_goal(&self, value: GoalLiveState) {
        self.goal.send_if_modified(|previous| {
            if previous
                .as_ref()
                .and_then(|previous| previous.as_ref().ok())
                == Some(&value)
            {
                return false;
            }
            *previous = Some(Ok(value));
            true
        });
    }
    /// Shares the already owned projection stream's latest retained snapshot.
    pub fn projection_changes(&self) -> tokio::sync::watch::Receiver<Option<ProjectionSnapshot>> {
        self.projections.subscribe()
    }
    /// Observes process-local driving, independently of durable Goal state.
    pub fn goal_changes(&self) -> tokio::sync::watch::Receiver<Option<Result<GoalLiveState>>> {
        self.goal.subscribe()
    }
    /// Returns an unresolved exact control; callers must not replace its identity.
    ///
    /// # Panics
    /// Panics if a prior panic poisoned pending control storage.
    pub fn pending_goal(&self) -> Option<GoalControl> {
        self.goal_pending
            .lock()
            .expect("Goal pending poisoned")
            .clone()
    }

    pub(super) fn start_goal(self: &Arc<Self>) {
        let controller = self.clone();
        self.execution.spawn(self.tasks.track_future(async move {
            let observing = async {
                let mut stream = crate::read_with_capacity_retry(&controller.execution, || controller.handle.observe_goal()).await?;
                while let Some(value) = stream.next().await {
                    let value = value?;
                    controller.publish_goal(value);
                }
                Err::<(), _>(SessionError::Backend("Goal observation ended; reconnect to refresh live state".into()))
            };
            tokio::select! { biased;
                () = controller.stop.cancelled() => {},
                result = observing => { if let Err(error) = result { controller.goal.send_replace(Some(Err(error))); } }
            }
        }));
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
        let Some(request) = self.pending_goal() else {
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
        {
            let mut pending = self.goal_pending.lock().expect("Goal pending poisoned");
            if pending
                .as_ref()
                .is_some_and(|pending| !query || *pending != request)
            {
                return Box::pin(async {
                    Err(SessionError::Invalid(
                        "Reconcile the unresolved Goal control first".into(),
                    ))
                });
            }
            *pending = Some(request.clone());
        }
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
            if !uncertain {
                let mut pending = controller.goal_pending.lock().expect("Goal pending poisoned");
                if pending.as_ref().is_some_and(|request| request.request_id == request_id) { pending.take(); }
            }
            if let Ok(receipt) = &result { controller.publish_goal(receipt.live.clone()); }
            if result.is_ok() || uncertain { controller.start_observing(ObservationCursor::default()); }
            result
        }));
        Box::pin(async move { task.await.unwrap_or(Err(fallback)) })
    }
}
