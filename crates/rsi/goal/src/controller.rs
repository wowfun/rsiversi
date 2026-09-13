//! Bounded Host ownership; all durable transitions remain with the Session bridge.

use crate::{
    GoalControl, GoalControlReceipt, GoalController, GoalControllerContract, GoalDriverStage,
    GoalError, GoalLiveState, GoalLiveStream, GoalResult, GoalSession, GoalSnapshot,
};
use async_trait::async_trait;
use futures_util::FutureExt;
use rsi_agent_goal::{GoalAction, GoalPhase};
use rsi_agent_session_protocol::{CommandRevision, DomainRequestId, SessionId};
use rsi_agent_turn_protocol::{
    ContinuationBinding, ContinuationLease, MessageState, SessionContinuationsContract,
    TurnServiceContract,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{Mutex as AsyncMutex, Semaphore, watch};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

mod driver;

/// One Host-generation controller with finite driver, operation and observer retention.
#[derive(Clone, Debug)]
pub struct GoalService {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    admission: Mutex<()>,
    sessions: Mutex<BTreeMap<SessionId, Arc<Owner>>>,
    operations: Arc<Semaphore>,
    observers: Arc<Semaphore>,
    tasks: TaskTracker,
    stopped: CancellationToken,
    cleanup_errors: Mutex<Vec<String>>,
}

#[derive(Debug)]
struct Owner {
    gate: AsyncMutex<()>,
    live: Mutex<Option<Live>>,
    status: watch::Sender<GoalLiveState>,
}

#[derive(Clone, Debug)]
struct Live {
    session: Arc<dyn GoalSession>,
    lease: ContinuationLease,
    stop: CancellationToken,
}

impl Default for GoalService {
    fn default() -> Self {
        Self {
            inner: Arc::new(Inner {
                admission: Mutex::new(()),
                sessions: Mutex::new(BTreeMap::new()),
                operations: Arc::new(Semaphore::new(64)),
                observers: Arc::new(Semaphore::new(64)),
                tasks: TaskTracker::new(),
                stopped: CancellationToken::new(),
                cleanup_errors: Mutex::new(Vec::new()),
            }),
        }
    }
}

impl GoalService {
    fn owner(&self, session: &SessionId) -> GoalResult<Arc<Owner>> {
        if self.inner.stopped.is_cancelled() {
            return Err(GoalError::ShuttingDown);
        }
        let mut owners = self
            .inner
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(owner) = owners.get(session) {
            return Ok(owner.clone());
        }
        if owners.len() >= 64 {
            owners.retain(|_, owner| {
                Arc::strong_count(owner) != 1
                    || owner.status.receiver_count() != 0
                    || owner
                        .live
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .is_some()
            });
        }
        if owners.len() >= 64 {
            return Err(GoalError::Capacity);
        }
        let owner = Arc::new(Owner {
            gate: AsyncMutex::new(()),
            live: Mutex::new(None),
            status: watch::channel(GoalLiveState::default()).0,
        });
        owners.insert(session.clone(), owner.clone());
        Ok(owner)
    }

    /// Revokes, cancels and joins every driver before its Kernel dependencies withdraw.
    ///
    /// # Errors
    /// Reports exact cleanup failures; it does not assert that failed settlement persisted.
    pub async fn stop(&self) -> GoalResult<()> {
        {
            let _admission = self
                .inner
                .admission
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.inner.stopped.cancel();
            self.inner.operations.close();
            self.inner.observers.close();
            self.inner.tasks.close();
        }
        let owners = self
            .inner
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for owner in owners {
            if let Some(live) = owner.live() {
                live.lease.revoke();
                live.stop.cancel();
            }
            owner.status.send_modify(|state| {
                state.available = false;
                state.armed = false;
            });
        }
        self.inner.tasks.wait().await;
        let errors = self
            .inner
            .cleanup_errors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(GoalError::Backend(errors.join("; ")))
        }
    }

    async fn control_owned(
        &self,
        owner: &Arc<Owner>,
        session: Arc<dyn GoalSession>,
        request: GoalControl,
    ) -> GoalResult<GoalControlReceipt> {
        let invocation = request.invocation()?;
        let starts = matches!(
            request.action,
            GoalAction::Create { .. } | GoalAction::Resume { .. }
        );
        let cancel = matches!(request.action, GoalAction::Cancel { .. });
        if !starts
            && let Some(live) = owner.live()
            && live.lease.binding().owner == *action_id(&request.action)
        {
            live.lease.revoke();
            owner.publish(
                &live,
                GoalDriverStage::Stopping,
                None,
                Some("Scheduling paused; admitted work is settling".into()),
            );
        }
        let _gate = owner.gate.lock().await;
        if self.inner.stopped.is_cancelled() {
            return Err(GoalError::ShuttingDown);
        }
        if starts && owner.live().is_some() {
            if let Some(receipt) = session.command_status(&request.request_id).await?
                && receipt.invocation_sha256() == invocation.digest().map_err(invalid)?
            {
                return Ok(GoalControlReceipt {
                    command: receipt,
                    live: owner.snapshot(),
                });
            }
            return Err(GoalError::Conflict);
        }
        let receipt = match session.application_command(invocation.clone()).await {
            Ok(receipt) => receipt,
            Err(error) => match session.command_status(&request.request_id).await {
                Ok(Some(receipt))
                    if receipt.invocation_sha256() == invocation.digest().map_err(invalid)? =>
                {
                    receipt
                }
                Ok(Some(_)) => return Err(GoalError::Conflict),
                Ok(None) => return Err(error),
                Err(_) => return Err(GoalError::OutcomeUnknown(request.request_id.to_string())),
            },
        };
        let completion = async {
            let snapshot = session.snapshot().await?;
            if starts {
                if snapshot.revision != receipt.revision() {
                    owner.status.send_replace(GoalLiveState {
                    detail: Some(
                        "The command was followed by newer state; resume from the current revision"
                            .into(),
                    ),
                    ..GoalLiveState::default()
                });
                    return Ok(GoalControlReceipt {
                        command: receipt,
                        live: owner.snapshot(),
                    });
                }
                let goal = snapshot
                    .state
                    .goal
                    .as_ref()
                    .filter(|goal| {
                        &goal.id == action_id(&request.action) && goal.phase == GoalPhase::Active
                    })
                    .ok_or(GoalError::Conflict)?;
                let lease = session.arm(binding(&snapshot, goal)?).await?;
                self.start(
                    owner,
                    Live {
                        session,
                        lease,
                        stop: self.inner.stopped.child_token(),
                    },
                )?;
            } else {
                self.pause_owned(owner, session, &snapshot, cancel).await?;
            }
            Ok(GoalControlReceipt {
                command: receipt,
                live: owner.snapshot(),
            })
        }
        .await;
        completion.map_err(|error: GoalError| {
            owner
                .status
                .send_modify(|state| state.detail = Some(bounded(&error.to_string())));
            GoalError::OutcomeUnknown(request.request_id.to_string())
        })
    }

    async fn pause_owned(
        &self,
        owner: &Arc<Owner>,
        session: Arc<dyn GoalSession>,
        snapshot: &GoalSnapshot,
        cancel: bool,
    ) -> GoalResult<()> {
        let Some(goal) = &snapshot.state.goal else {
            return Ok(());
        };
        let Some(reservation) = goal
            .reservation
            .as_ref()
            .filter(|reservation| reservation.settlement.is_none())
        else {
            return Ok(());
        };
        let receipt = session.message_status(&reservation.message_id).await?;
        if receipt.is_none() && !cancel {
            return Ok(());
        }
        let existing = owner.live();
        let live = if let Some(live) = existing.clone() {
            live
        } else {
            let lease = session
                .retain_for_settlement(binding(snapshot, goal)?)
                .await?;
            Live {
                session: session.clone(),
                lease,
                stop: self.inner.stopped.child_token(),
            }
        };
        live.lease.revoke();
        let Some(receipt) = receipt else {
            self.settle(
                owner,
                &live,
                &reservation.message_id,
                rsi_agent_goal::RoundSettlement::Abandoned,
            )
            .await?;
            owner.publish(&live, GoalDriverStage::Disarmed, None, None);
            return Ok(());
        };
        if receipt.state == MessageState::Pending {
            session
                .discard_if_pending(&live.lease, &reservation.message_id)
                .await?;
        }
        if cancel {
            session.cancel(&reservation.message_id).await?;
        }
        if existing.is_none() {
            self.start(owner, live)?;
        }
        Ok(())
    }

    fn start(&self, owner: &Arc<Owner>, live: Live) -> GoalResult<()> {
        let _admission = self
            .inner
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.inner.stopped.is_cancelled() {
            live.lease.revoke();
            return Err(GoalError::ShuttingDown);
        }
        *owner
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(live.clone());
        owner.publish(
            &live,
            if live.lease.is_armed() {
                GoalDriverStage::Reserving
            } else {
                GoalDriverStage::Stopping
            },
            None,
            None,
        );
        let owner = owner.clone();
        let controller = self.clone();
        self.inner.tasks.spawn(async move {
            let result = std::panic::AssertUnwindSafe(controller.drive(&owner, &live))
                .catch_unwind()
                .await
                .unwrap_or_else(|_| Err(GoalError::Backend("Goal driver panicked".into())));
            let preempted = matches!(result, Err(GoalError::Disarmed));
            let result = match result {
                Err(GoalError::Disarmed) => Ok(()),
                result => result,
            };
            live.lease.revoke();
            let result = if live.stop.is_cancelled() {
                let cleanup = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    controller.finish_shutdown(&owner, &live),
                )
                .await
                .unwrap_or_else(|_| {
                    Err(GoalError::Backend(
                        "Goal cleanup exceeded its deadline".into(),
                    ))
                });
                match cleanup {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        controller
                            .inner
                            .cleanup_errors
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(bounded(&error.to_string()));
                        Err(error)
                    }
                }
            } else {
                result
            };
            let _gate = owner.gate.lock().await;
            owner.publish(
                &live,
                if result.is_ok() {
                    GoalDriverStage::Disarmed
                } else {
                    GoalDriverStage::Failed
                },
                None,
                result.err().map(|error| bounded(&error.to_string())).or_else(|| preempted.then(|| "Automatic scheduling was preempted. Resume Goal to reconcile the charged round, or Cancel to abandon unaccepted input.".into())),
            );
            *owner
                .live
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        });
        Ok(())
    }
}

impl Owner {
    fn live(&self) -> Option<Live> {
        self.live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    fn snapshot(&self) -> GoalLiveState {
        let mut state = self.status.borrow().clone();
        state.armed = self.live().is_some_and(|live| live.lease.is_armed());
        state
    }
    fn publish(
        &self,
        live: &Live,
        stage: GoalDriverStage,
        message_id: Option<rsi_agent_session_protocol::MessageId>,
        detail: Option<String>,
    ) {
        self.status.send_replace(GoalLiveState {
            available: !live.stop.is_cancelled(),
            armed: live.lease.is_armed(),
            stage,
            message_id,
            detail,
        });
    }
}

fn admission_error(error: &tokio::sync::TryAcquireError) -> GoalError {
    match error {
        tokio::sync::TryAcquireError::Closed => GoalError::ShuttingDown,
        tokio::sync::TryAcquireError::NoPermits => GoalError::Capacity,
    }
}

#[async_trait]
impl GoalController for GoalService {
    async fn control(
        &self,
        session: Arc<dyn GoalSession>,
        request: GoalControl,
    ) -> GoalResult<GoalControlReceipt> {
        request.invocation()?;
        let permit = self
            .inner
            .operations
            .clone()
            .try_acquire_owned()
            .map_err(|error| admission_error(&error))?;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let controller = self.clone();
        {
            let _admission = self
                .inner
                .admission
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let owner = self.owner(session.session_id())?;
            self.inner.tasks.spawn(async move {
                let _permit = permit;
                let request_id = request.request_id.to_string();
                let call = std::panic::AssertUnwindSafe(controller.control_owned(&owner, session, request)).catch_unwind();
                let result = tokio::select! {
                    biased;
                    () = controller.inner.stopped.cancelled() => Err(GoalError::OutcomeUnknown(request_id.clone())),
                    result = tokio::time::timeout(std::time::Duration::from_secs(30), call) => match result {
                        Ok(Ok(result)) => result,
                        Ok(Err(_)) => Err(GoalError::OutcomeUnknown(request_id.clone())),
                        Err(_) => Err(GoalError::OutcomeUnknown(request_id)),
                    }
                };
                let _ = sender.send(result);
            });
        }
        receiver
            .await
            .map_err(|_| GoalError::Backend("Goal control result owner disappeared".into()))?
    }

    fn status(&self, session: &SessionId) -> GoalResult<GoalLiveState> {
        if self.inner.stopped.is_cancelled() {
            return Err(GoalError::ShuttingDown);
        }
        Ok(self
            .inner
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session)
            .map_or_else(GoalLiveState::default, |owner| owner.snapshot()))
    }

    fn observe(&self, session: &SessionId) -> GoalResult<GoalLiveStream> {
        let permit = self
            .inner
            .observers
            .clone()
            .try_acquire_owned()
            .map_err(|error| admission_error(&error))?;
        let owner = self.owner(session)?;
        let mut receiver = owner.status.subscribe();
        let stopped = self.inner.stopped.clone();
        Ok(Box::pin(async_stream::try_stream! {
            let _permit = permit;
            loop {
                receiver.borrow_and_update();
                yield owner.snapshot();
                tokio::select! {
                    () = stopped.cancelled() => { yield GoalLiveState { available: false, ..GoalLiveState::default() }; break; }
                    changed = receiver.changed() => if changed.is_err() { break; },
                }
            }
        }))
    }
}

fn action_id(action: &GoalAction) -> &DomainRequestId {
    match action {
        GoalAction::Create { id, .. }
        | GoalAction::Resume { id }
        | GoalAction::Pause { id }
        | GoalAction::Cancel { id } => id,
    }
}

fn binding(
    snapshot: &GoalSnapshot,
    goal: &rsi_agent_goal::Goal,
) -> GoalResult<ContinuationBinding> {
    Ok(ContinuationBinding {
        domain: snapshot.domain.snapshot.identity().clone(),
        owner: goal.id.clone(),
        revision: snapshot.domain.revision,
        snapshot_sha256: snapshot.domain.snapshot.sha256().map_err(invalid)?,
        initial_input: if matches!(snapshot.revision, CommandRevision::Draft { .. }) {
            goal.reservation
                .as_ref()
                .map(|reservation| reservation.input(&goal.id))
        } else {
            None
        },
    })
}
fn invalid(error: impl std::fmt::Display) -> GoalError {
    GoalError::Invalid(bounded(&error.to_string()))
}
fn bounded(text: &str) -> String {
    let mut output = String::new();
    for character in text
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
    {
        if output.len() + character.len_utf8() > 4096 {
            break;
        }
        output.push(character);
    }
    output
}

/// Ordinary Host plugin; the declared Kernel dependencies outlive its driver cleanup.
#[derive(Clone, Copy, Debug, Default)]
pub struct GoalControllerFactory;

#[async_trait]
impl PluginFactory for GoalControllerFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "Goal controller configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<SessionContinuationsContract>()
            .requiring_local::<TurnServiceContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let continuations = plan.local::<SessionContinuationsContract>()?;
        let turns = plan.local::<TurnServiceContract>()?;
        let service = Arc::new(GoalService::default());
        let supply = plan
            .context()
            .provide_local::<GoalControllerContract>(service.clone())?;
        plan.defer(
            "stop Goal controllers",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    let result = service.stop().await.map_err(|error| error.to_string());
                    drop((continuations, turns));
                    result
                })
            }),
        )
    }
}
