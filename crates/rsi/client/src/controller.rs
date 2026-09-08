use crate::{
    ObservationSink, ObservationSinkContract, observe_interactions, observe_session,
    submit_with_reconciliation,
};
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    session_id: SessionId,
    #[serde(default)]
    cursor: Option<ObservationCursor>,
}

#[derive(Debug, Default)]
struct Admission {
    observing: bool,
}

/// Surface-local submission and observation owner, created only by its plugin.
#[derive(Debug)]
pub struct SessionController {
    session_id: SessionId,
    handle: Arc<dyn SessionHandle>,
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
                controller.start_observing(ObservationCursor::default());
            }
            result
        }));
        drop(admission);
        Box::pin(async move { task.await.unwrap_or(Err(unknown)) })
    }

    fn start_observing(self: &Arc<Self>, cursor: ObservationCursor) {
        let mut admission = self
            .admission
            .lock()
            .expect("controller admission poisoned");
        if admission.observing || self.stop.is_cancelled() {
            return;
        }
        admission.observing = true;
        for facts in [true, false] {
            let controller = self.clone();
            drop(self.execution.spawn(self.tasks.track_future(async move {
                let observing = async {
                    if facts {
                        observe_session(
                            controller.handle.as_ref(),
                            cursor,
                            controller.sink.as_ref(),
                            &controller.execution,
                        )
                        .await
                    } else {
                        observe_interactions(
                            controller.handle.as_ref(),
                            controller.sink.as_ref(),
                            &controller.execution,
                        )
                        .await
                    }
                };
                tokio::select! { biased;
                    () = controller.stop.cancelled() => {},
                    result = observing => {
                        if let Err(error) = result {
                            tokio::select! { biased;
                                () = controller.stop.cancelled() => {},
                                () = controller.sink.stopped(if facts { crate::ObservationKind::Facts } else { crate::ObservationKind::Interactions }, &error) => {},
                            }
                        }
                    },
                }
            })));
        }
    }

    fn retire(&self) {
        {
            let _admission = self
                .admission
                .lock()
                .expect("controller admission poisoned");
            self.stop.cancel();
            self.submissions.close();
            self.tasks.close();
        }
    }

    fn start_projections(self: &Arc<Self>) {
        let controller = self.clone();
        drop(self.execution.spawn(self.tasks.track_future(async move {
            tokio::select! { biased;
                () = controller.stop.cancelled() => {},
                result = crate::observe_projections(controller.handle.as_ref(), controller.sink.as_ref(), &controller.execution) => {
                    if let Err(error) = result {
                        tokio::select! { biased;
                            () = controller.stop.cancelled() => {},
                            () = controller.sink.stopped(crate::ObservationKind::Projections, &error) => {},
                        }
                    }
                },
            }
        })));
    }

    async fn close(&self) {
        self.retire();
        self.tasks.wait().await;
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
            session_id: config.session_id,
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
        controller.start_projections();
        if let Some(cursor) = config.cursor {
            controller.start_observing(cursor);
        }
        Ok(())
    }
}
