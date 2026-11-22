use crate::{ObservationSink, ObservationSinkContract, submit_with_reconciliation};
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_agent_session_protocol::SessionId;
use rsi_agent_turn_protocol::{MessageReceipt, ObservationCursor};
use rsi_meta::{
    ActivationPlan, ConfigValue, Execution, MetaError, PluginFactory, PreparedActivation,
};
use rsi_session_protocol::{Result, SessionContract, SessionError, SessionHandle, SubmitInput};
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[path = "commands.rs"]
pub(super) mod commands;
#[path = "resources.rs"]
pub(super) mod resources;
#[path = "source.rs"]
pub(super) mod source;
#[path = "surface.rs"]
mod surface;
#[path = "tasks.rs"]
mod task_status;
pub use task_status::GoalControlState;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    session_id: SessionId,
    #[serde(default)]
    cursor: Option<ObservationCursor>,
    #[serde(default)]
    defer_observation: bool,
}

#[derive(Debug, Default)]
struct Admission {
    observing: bool,
    state_observing: bool,
}

/// Surface-local submission and observation owner, created only by its plugin.
#[derive(Debug)]
pub struct SessionController {
    projections: tokio::sync::watch::Sender<Option<rsi_session_protocol::ProjectionSnapshot>>,
    goal: tokio::sync::watch::Sender<Option<rsi_session_protocol::Result<rsi_goal::GoalLiveState>>>,
    goal_control: tokio::sync::watch::Sender<GoalControlState>,
    pub(super) workflow_selection: Mutex<super::workflow::WorkflowSelection>,
    pub(super) workflow_changed: tokio::sync::watch::Sender<u64>,
    session_id: SessionId,
    pub(super) handle: Arc<dyn SessionHandle>,
    can_stop_work: tokio::sync::OnceCell<bool>,
    sink: Arc<dyn ObservationSink>,
    execution: Execution,
    stop: CancellationToken,
    tasks: TaskTracker,
    submissions: Arc<Semaphore>,
    admission: Mutex<Admission>,
}

impl SessionController {
    /// Exact Session identity; changing selection creates a new plugin generation.
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Reads the presentation permission once for this attached credential.
    /// The Host authorizes each stop against its current owner.
    pub async fn can_stop_work(&self) -> Result<bool> {
        self.can_stop_work
            .get_or_try_init(|| self.handle.can_stop_work())
            .await
            .copied()
    }

    /// Shares this surface's existing Session binding with sibling UI contributions.
    pub fn session_handle(&self) -> Arc<dyn SessionHandle> {
        self.handle.clone()
    }

    /// Admits reconciliation before returning a waiter; Drop never replays or cancels it.
    ///
    /// # Panics
    /// Panics if a prior panic poisoned the controller's admission state.
    pub fn submit(
        self: &Arc<Self>,
        request: SubmitInput,
    ) -> BoxFuture<'static, Result<MessageReceipt>> {
        self.start_submission(request, false, None)
    }

    /// Owns submission until completion or explicit cancellation of reconciliation.
    /// Cancellation returns the original unknown-outcome identity, never proof of
    /// non-execution. It does not cancel independently admitted domain work.
    pub fn submit_cancellable(
        self: &Arc<Self>,
        request: SubmitInput,
        stop: CancellationToken,
    ) -> BoxFuture<'static, Result<MessageReceipt>> {
        self.start_submission(request, false, Some(stop))
    }

    /// Resolves a retained uncertain request before permitting an identical retry.
    /// Query failure keeps its outcome unknown without sending another mutation.
    pub fn retry(
        self: &Arc<Self>,
        request: SubmitInput,
    ) -> BoxFuture<'static, Result<MessageReceipt>> {
        self.start_submission(request, true, None)
    }

    fn start_submission(
        self: &Arc<Self>,
        request: SubmitInput,
        retry: bool,
        stop: Option<CancellationToken>,
    ) -> BoxFuture<'static, Result<MessageReceipt>> {
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
        let unknown = SessionError::MessageOutcomeUnknown {
            session: self.session_id.to_string(),
            message: request.message_id.to_string(),
        };
        let controller = self.clone();
        let unresolved = unknown.clone();
        let task = self.execution.spawn(self.tasks.track_future(async move {
            let _permit = permit;
            let reconcile = async { if retry {
                match crate::read_with_capacity_retry(&controller.execution, || {
                    controller.handle.message_status(&request.message_id)
                })
                .await
                {
                    Ok(receipt) => Ok(receipt),
                    Err(SessionError::NotFound(_)) => {
                        submit_with_reconciliation(controller.handle.as_ref(), request).await
                    }
                    Err(_) => Err(unresolved.clone()),
                }
            } else {
                submit_with_reconciliation(controller.handle.as_ref(), request).await
            } };
            let result = tokio::select! { biased;
                () = async { match stop { Some(stop) => stop.cancelled().await, None => std::future::pending().await } } => Err(unresolved),
                result = reconcile => result,
            };
            if result.is_ok() {
                controller.observe_durable_session();
            }
            result
        }));
        drop(admission);
        Box::pin(async move { task.await.unwrap_or(Err(unknown)) })
    }

    /// Starts the surface's observers after its first confirmed durable receipt.
    /// An already observing or retired controller keeps its existing state.
    pub fn observe_durable_session(self: &Arc<Self>) {
        self.start_observing(ObservationCursor::default());
    }

    /// Starts observation after an inspected cut. The first start owns the cursor.
    ///
    /// # Panics
    /// Panics if a prior panic poisoned controller admission.
    pub fn start_observing(self: &Arc<Self>, cursor: ObservationCursor) {
        let mut admission = self
            .admission
            .lock()
            .expect("controller admission poisoned");
        if admission.observing || self.stop.is_cancelled() {
            return;
        }
        admission.observing = true;
        let view = if admission.state_observing {
            rsi_session_protocol::SurfaceView::published(cursor)
        } else {
            rsi_session_protocol::SurfaceView::attached(cursor)
        };
        admission.state_observing = true;
        self.spawn_feed(
            if self.sink.wants_background_jobs() {
                view.with_background_jobs()
            } else {
                view
            },
            None,
        );
    }

    /// Starts Goal and projection observation as soon as the unpublished draft is attached.
    ///
    /// # Panics
    /// Panics if a previous panic poisoned controller admission.
    pub fn start_draft_observation(self: &Arc<Self>) {
        let mut admission = self
            .admission
            .lock()
            .expect("controller admission poisoned");
        if self.stop.is_cancelled() || admission.state_observing {
            return;
        }
        admission.state_observing = true;
        self.spawn_feed(rsi_session_protocol::SurfaceView::draft(), None);
    }

    fn spawn_feed(
        self: &Arc<Self>,
        view: rsi_session_protocol::SurfaceView,
        failed: Option<(crate::ObservationKind, crate::ObservationFailure)>,
    ) {
        let controller = self.clone();
        drop(self.execution.spawn(self.tasks.track_future(async move {
            tokio::select! { biased;
                () = controller.stop.cancelled() => {},
                () = controller.follow_surface(view, failed) => {},
            }
        })));
    }

    fn retire(&self) {
        {
            let _admission = self
                .admission
                .lock()
                .expect("controller admission poisoned");
            self.stop.cancel();
            self.projections.send_replace(None);
            self.goal.send_replace(None);
            self.submissions.close();
            self.tasks.close();
        }
    }

    async fn close(&self) {
        self.retire();
        self.tasks.wait().await;
        self.projections.send_replace(None);
        self.goal.send_replace(None);
    }
}

/// Nominal Local contract for one surface's Session controller generation.
#[derive(Debug)]
pub struct SessionControllerContract;
impl rsi_meta::LocalContract for SessionControllerContract {
    const KEY: &'static str = "rsi.client.session-controller";
    type Service = SessionController;
}

/// Ordinary Meta owner of shared native/browser Session application control.
#[derive(Clone, Debug, Default)]
pub struct SessionControllerFactory;
#[async_trait]
impl PluginFactory for SessionControllerFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: Configuration = serde_json::from_value(desired.clone())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let bytes = std::mem::size_of::<Configuration>() + config.session_id.as_str().len();
        Ok(
            PreparedActivation::with_state(desired.clone(), config, bytes)
                .requiring_local::<SessionContract>()
                .requiring_local::<ObservationSinkContract>(),
        )
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<Configuration>()?;
        let service = plan.local::<SessionContract>()?;
        let handle = crate::read_with_capacity_retry(plan.context().runtime().execution(), || {
            service.attach(&config.session_id)
        })
        .await
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        let controller = Arc::new(SessionController {
            projections: tokio::sync::watch::channel(None).0,
            goal: tokio::sync::watch::channel(None).0,
            goal_control: tokio::sync::watch::channel(GoalControlState::Idle).0,
            workflow_selection: Mutex::new(super::workflow::WorkflowSelection::Latest),
            workflow_changed: tokio::sync::watch::channel(0).0,
            session_id: config.session_id,
            can_stop_work: tokio::sync::OnceCell::new(),
            handle,
            sink: plan.local::<ObservationSinkContract>()?,
            execution: plan.context().runtime().execution().clone(),
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
            submissions: Arc::new(Semaphore::new(4)),
            admission: Mutex::new(Admission::default()),
        });
        let cleanup = controller.clone();
        plan.defer(
            "close Session controller",
            Box::new(move || {
                Box::pin(async move {
                    cleanup.close().await;
                    Ok(())
                })
            }),
        )?;
        let supply = plan
            .context()
            .provide_local::<SessionControllerContract>(controller.clone())?;
        let withdrawing = controller.clone();
        plan.defer(
            "withdraw Session controller",
            Box::new(move || {
                Box::pin(async move {
                    withdrawing.retire();
                    drop(supply);
                    Ok(())
                })
            }),
        )?;
        if let Some(cursor) = config.cursor {
            controller.start_observing(cursor);
        } else if !config.defer_observation {
            controller.start_draft_observation();
        }
        Ok(())
    }
}
