//! Native Host ownership for bounded reminders.
#![deny(unsafe_code)]
#![warn(missing_docs)]
use async_trait::async_trait;
use futures_util::{FutureExt, StreamExt};
use rsi_agent_schedule::{
    ReserveSchedule, SCHEDULE_DOMAIN, SCHEDULE_RESERVE, SCHEDULE_SETTLE, ScheduleClock,
    ScheduleController, ScheduleControllerContract, ScheduleEpoch, ScheduleState, SettleSchedule,
    mutation_id, owner, request_id,
};
use rsi_agent_session_protocol::{
    CommandArguments, ContinuationInput, ContributionId, DomainIdentity, DomainMutationSource,
    DomainStateView, MessageId, SessionCommandInvocation, SessionId,
};
use rsi_agent_turn_protocol::{
    AgentCallerAuthority, ContinuationBinding, ContinuationLease, DomainMutationReceipt,
    MessageState, SessionCommands, SessionCommandsContract, SessionContinuations,
    SessionContinuationsContract, SessionProjections, SessionProjectionsContract, SubmitSession,
    TurnError, TurnExecution, TurnExecutionContract, TurnService, TurnServiceContract,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use std::result::Result;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// Production UTC clock. Long timers recheck wall time at most a minute apart.
#[derive(Debug, Default)]
pub struct UtcClock;
#[async_trait]
impl ScheduleClock for UtcClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| u64::try_from(d.as_millis()).ok())
            .unwrap_or(0)
    }
    async fn wait_until(&self, at_ms: u64, cancellation: CancellationToken) {
        loop {
            let remaining = at_ms.saturating_sub(self.now_ms());
            if remaining == 0 {
                return;
            }
            tokio::select! { () = cancellation.cancelled() => return, () = tokio::time::sleep(Duration::from_millis(remaining.min(60_000))) => {} }
        }
    }
}
/// One generation's bounded set of live timer owners.
#[derive(Clone, Debug)]
pub struct ScheduleService {
    inner: Arc<Inner>,
}
#[derive(Debug)]
struct Inner {
    turns: Arc<dyn TurnService>,
    execution: Arc<dyn TurnExecution>,
    commands: Arc<dyn SessionCommands>,
    continuations: Arc<dyn SessionContinuations>,
    projections: Arc<dyn SessionProjections>,
    clock: Arc<dyn ScheduleClock>,
    epoch: ScheduleEpoch,
    stop: CancellationToken,
    gate: Mutex<()>,
    session_gates: Mutex<BTreeMap<SessionId, Weak<AsyncMutex<()>>>>,
    arming: Arc<Semaphore>,
    failures: Mutex<BTreeMap<SessionId, String>>,
    owners: Mutex<BTreeMap<SessionId, Arc<Live>>>,
    capacity: Arc<Semaphore>,
    tasks: TaskTracker,
}
#[derive(Debug)]
struct Live {
    lease: ContinuationLease,
    stop: CancellationToken,
    done: CancellationToken,
    _capacity: tokio::sync::OwnedSemaphorePermit,
}
impl ScheduleService {
    /// Builds an isolated controller with an injectable UTC clock.
    pub fn new(
        turns: Arc<dyn TurnService>,
        execution: Arc<dyn TurnExecution>,
        commands: Arc<dyn SessionCommands>,
        continuations: Arc<dyn SessionContinuations>,
        projections: Arc<dyn SessionProjections>,
        clock: Arc<dyn ScheduleClock>,
    ) -> Self {
        let stop = CancellationToken::new();
        Self {
            inner: Arc::new(Inner {
                turns,
                execution,
                commands,
                continuations,
                projections,
                clock,
                epoch: ScheduleEpoch::new(stop.clone()),
                stop,
                gate: Mutex::new(()),
                session_gates: Mutex::new(BTreeMap::new()),
                arming: Arc::new(Semaphore::new(64)),
                failures: Mutex::new(BTreeMap::new()),
                owners: Mutex::new(BTreeMap::new()),
                capacity: Arc::new(Semaphore::new(64)),
                tasks: TaskTracker::new(),
            }),
        }
    }
    /// Closes admission and joins all owned drivers before dependencies withdraw.
    ///
    /// # Errors
    /// Reports a driver that fails to settle within the shutdown deadline.
    ///
    /// # Panics
    /// Panics if a controller registry mutex was poisoned by a previous panic.
    pub async fn stop(&self) -> Result<(), String> {
        {
            let _gate = self.inner.gate.lock().unwrap();
            self.inner.stop.cancel();
            for live in self.inner.owners.lock().unwrap().values() {
                live.lease.revoke();
                live.stop.cancel();
            }
            self.inner.tasks.close();
        }
        tokio::time::timeout(Duration::from_secs(30), self.inner.tasks.wait())
            .await
            .map_err(|_| "Schedule cleanup timed out")?;
        self.inner.owners.lock().unwrap().clear();
        let failures = self.inner.failures.lock().unwrap();
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "Schedule driver or cleanup failed for {} Session(s)",
                failures.len()
            ))
        }
    }
    async fn snapshot(
        &self,
        session: &SessionId,
    ) -> Result<(DomainStateView, ScheduleState), TurnError> {
        let view = self
            .inner
            .turns
            .domain_states(session)
            .await?
            .into_iter()
            .find(|v| v.snapshot.identity().id() == SCHEDULE_DOMAIN)
            .ok_or_else(|| TurnError::Invalid("Schedule domain unavailable".into()))?;
        if view.snapshot.identity().version() != 1 {
            return Err(TurnError::Invalid("unsupported Schedule codec".into()));
        }
        let state: ScheduleState = serde_json::from_value(view.snapshot.state().value().clone())
            .map_err(|e| TurnError::Invalid(display(e)))?;
        state.validate().map_err(TurnError::Invalid)?;
        Ok((view, state))
    }
    async fn command(
        &self,
        live: &Live,
        reserve: Option<(u64, ContinuationInput)>,
        message: Option<MessageId>,
    ) -> Result<(), TurnError> {
        retry_contention(|| self.command_once(live, reserve.clone(), message.clone())).await
    }
    async fn command_once(
        &self,
        live: &Live,
        reserve: Option<(u64, ContinuationInput)>,
        message: Option<MessageId>,
    ) -> Result<(), TurnError> {
        let session = live.lease.session_id();
        let revision = self
            .inner
            .commands
            .list(self.inner.turns.prepare_resume(session).await?)
            .await?
            .revision();
        let (command, id, arguments, input) = if let Some((now_ms, input)) = reserve {
            (
                SCHEDULE_RESERVE,
                request_id(input.round, "reserve").map_err(TurnError::Invalid)?,
                serde_json::to_value(ReserveSchedule { now_ms }).expect("bounded UTC value"),
                Some(input),
            )
        } else {
            let (_, state) = self.snapshot(session).await?;
            (
                SCHEDULE_SETTLE,
                request_id(state.allocated_rounds, "settle").map_err(TurnError::Invalid)?,
                serde_json::to_value(SettleSchedule {
                    message_id: message.expect("settlement input"),
                })
                .expect("bounded message identity"),
                None,
            )
        };
        self.inner
            .continuations
            .execute(
                &live.lease,
                self.inner.turns.prepare_resume(session).await?,
                SessionCommandInvocation {
                    command: ContributionId::new(command)
                        .map_err(|e| TurnError::Invalid(e.to_string()))?,
                    request_id: id,
                    expected_revision: revision,
                    arguments: CommandArguments::new(arguments)
                        .map_err(|e| TurnError::Invalid(e.to_string()))?,
                },
                input,
            )
            .await?;
        Ok(())
    }
    async fn drive(&self, live: &Live) -> Result<(), String> {
        loop {
            if live.stop.is_cancelled() {
                return Ok(());
            }
            let (_, mut state) = retry_contention(|| self.snapshot(live.lease.session_id()))
                .await
                .map_err(display)?;
            if let Some(reservation) = state.reservation.as_ref().filter(|r| !r.settled) {
                self.wait_terminal(live, &reservation.input.message_id, live.stop.clone())
                    .await?;
                self.command(live, None, Some(reservation.input.message_id.clone()))
                    .await
                    .map_err(display)?;
                continue;
            }
            let Some(due) = state.next_due() else {
                return Ok(());
            };
            self.inner.clock.wait_until(due, live.stop.clone()).await;
            if live.stop.is_cancelled() {
                return Ok(());
            }
            let now = self.inner.clock.now_ms();
            if now < due {
                continue;
            }
            let input = state.reserve(now)?;
            match self.command(live, Some((now, input)), None).await {
                Ok(()) => {}
                Err(TurnError::SessionBusy) => self
                    .inner
                    .continuations
                    .wait_idle(&live.lease, live.stop.clone())
                    .await
                    .map_err(display)?,
                Err(error) => return Err(display(error)),
            }
        }
    }
    async fn validate_receipt(
        &self,
        caller: &AgentCallerAuthority,
        receipt: &DomainMutationReceipt,
    ) -> Result<(), String> {
        let effect = caller
            .tool_effect_id()
            .ok_or("Schedule arm requires started Tool")?;
        if self
            .inner
            .execution
            .tool_caller(caller.claim(), effect)
            .map_err(display)?
            != *caller
            || receipt.session_id() != caller.session_id()
            || receipt.commit().request_id() != Some(&mutation_id(effect).map_err(display)?)
            || !matches!(receipt.commit().source(), DomainMutationSource::Turn { turn_id } if turn_id == caller.turn_id())
        {
            return Err("Schedule receipt does not belong to its exact Tool".into());
        }
        let canonical = self
            .inner
            .turns
            .domain_request(
                caller.session_id(),
                receipt.commit().request_id().expect("checked request"),
            )
            .await
            .map_err(display)?;
        if canonical.as_ref() != Some(receipt) {
            return Err("Schedule receipt is not canonical".into());
        }
        Ok(())
    }
    async fn wait_terminal(
        &self,
        live: &Live,
        message: &MessageId,
        stop: CancellationToken,
    ) -> Result<(), String> {
        let mut changes = self
            .inner
            .projections
            .watch_projection_changes(live.lease.session_id())
            .map_err(display)?;
        loop {
            let receipt = retry_contention(|| {
                self.inner
                    .turns
                    .message_status(live.lease.session_id(), message)
            })
            .await
            .map_err(display)?;
            let terminal = match receipt.state {
                MessageState::Pending => false,
                MessageState::Discarded { .. } => true,
                MessageState::Claimed { turn_id, .. } => {
                    retry_contention(|| self.inner.turns.outcome(live.lease.session_id(), &turn_id))
                        .await
                        .map_err(display)?
                        .is_some()
                }
            };
            if terminal {
                break;
            }
            tokio::select! {
                () = stop.cancelled() => return Err("Schedule wait cancelled".into()),
                next = changes.next() => if next.is_none() { return Err("Schedule observation ended".into()); },
            }
        }
        Ok(())
    }
    async fn finish(&self, live: &Live) -> Result<(), String> {
        live.lease.revoke();
        let (_, state) = retry_contention(|| self.snapshot(live.lease.session_id()))
            .await
            .map_err(display)?;
        if let Some(reservation) = state.reservation.filter(|r| !r.settled) {
            let receipt = self
                .inner
                .continuations
                .discard_if_pending(&live.lease, &reservation.input.message_id)
                .await
                .map_err(display)?;
            if let MessageState::Claimed { turn_id, .. } = receipt.state {
                self.inner
                    .turns
                    .cancel(live.lease.session_id(), &turn_id, None)
                    .await
                    .map_err(display)?;
            }
            self.wait_terminal(
                live,
                &reservation.input.message_id,
                CancellationToken::new(),
            )
            .await?;
            self.command(live, None, Some(reservation.input.message_id))
                .await
                .map_err(display)?;
        }
        Ok(())
    }
    fn record_failure(&self, session: &SessionId, problem: &str) {
        let mut failures = self.inner.failures.lock().unwrap();
        if !failures.contains_key(session) && failures.len() == 64 {
            failures.pop_first();
        }
        failures.insert(session.clone(), problem.chars().take(4096).collect());
    }
}
#[async_trait]
impl ScheduleController for ScheduleService {
    fn epoch(&self) -> ScheduleEpoch {
        self.inner.epoch.clone()
    }
    fn clock(&self) -> Arc<dyn ScheduleClock> {
        self.inner.clock.clone()
    }
    fn armed(&self, session: &SessionId) -> bool {
        self.inner
            .owners
            .lock()
            .unwrap()
            .get(session)
            .is_some_and(|live| live.lease.is_armed() && !live.done.is_cancelled())
    }
    fn failure(&self, session: &SessionId) -> Option<String> {
        self.inner.failures.lock().unwrap().get(session).cloned()
    }
    async fn disarm_for_mutation(
        &self,
        epoch: &ScheduleEpoch,
        session: &SessionId,
    ) -> Result<bool, String> {
        if !epoch.same_epoch(&self.inner.epoch) || !epoch.is_open() {
            return Ok(false);
        }
        let live = self.inner.owners.lock().unwrap().get(session).cloned();
        if let Some(live) = live {
            live.lease.revoke();
            live.stop.cancel();
            tokio::select! {
                () = self.inner.stop.cancelled() => return Ok(false),
                result = tokio::time::timeout(Duration::from_secs(25), live.done.cancelled()) => {
                    result.map_err(|_| "previous Schedule owner did not stop")?;
                }
            }
        }
        Ok(epoch.is_open())
    }
    async fn arm_after_commit(
        &self,
        epoch: &ScheduleEpoch,
        caller: &AgentCallerAuthority,
        domain: &DomainIdentity,
        receipt: &DomainMutationReceipt,
        cancellation: CancellationToken,
    ) -> Result<bool, String> {
        let (_tracked, _capacity) = {
            let _gate = self.inner.gate.lock().unwrap();
            if !epoch.same_epoch(&self.inner.epoch)
                || !epoch.is_open()
                || cancellation.is_cancelled()
            {
                return Ok(false);
            }
            (
                self.inner.tasks.token(),
                self.inner
                    .arming
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| "Schedule arming capacity exhausted")?,
            )
        };
        let gate = {
            let mut gates = self.inner.session_gates.lock().unwrap();
            gates.retain(|_, gate| gate.strong_count() > 0);
            if let Some(gate) = gates.get(caller.session_id()).and_then(Weak::upgrade) {
                gate
            } else {
                let gate = Arc::new(AsyncMutex::new(()));
                gates.insert(caller.session_id().clone(), Arc::downgrade(&gate));
                gate
            }
        };
        tokio::select! { biased;
            () = self.inner.stop.cancelled() => Ok(false),
            () = cancellation.cancelled() => Ok(false),
            result = async {
                let _session = gate.lock().await;
                self.arm(caller, domain, receipt, epoch, &cancellation).await
            } => result,
        }
    }
}
impl ScheduleService {
    async fn arm(
        &self,
        caller: &AgentCallerAuthority,
        domain: &DomainIdentity,
        receipt: &DomainMutationReceipt,
        epoch: &ScheduleEpoch,
        cancellation: &CancellationToken,
    ) -> Result<bool, String> {
        self.validate_receipt(caller, receipt).await?;
        let (view, state) = self.snapshot(caller.session_id()).await.map_err(display)?;
        if receipt.commit().updates().len() != 1
            || view.snapshot.identity() != domain
            || receipt.commit().updates()[0].revision() != view.revision
            || receipt.commit().updates()[0].snapshot() != &view.snapshot
        {
            return Ok(false);
        }
        let old = self
            .inner
            .owners
            .lock()
            .unwrap()
            .remove(caller.session_id());
        if let Some(old) = old {
            old.lease.revoke();
            old.stop.cancel();
            tokio::time::timeout(Duration::from_secs(25), old.done.cancelled())
                .await
                .map_err(|_| "previous Schedule owner did not stop")?;
            drop(old);
        }
        self.inner
            .owners
            .lock()
            .unwrap()
            .retain(|_, live| !live.done.is_cancelled());
        if !epoch.is_open()
            || cancellation.is_cancelled()
            || (state.next_due().is_none() && state.reservation.as_ref().is_none_or(|r| r.settled))
        {
            return Ok(false);
        }
        let permit = self
            .inner
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| "Schedule controller capacity exhausted")?;
        let lease = self
            .inner
            .continuations
            .arm(
                SubmitSession::Resume(
                    self.inner
                        .turns
                        .prepare_resume(caller.session_id())
                        .await
                        .map_err(display)?,
                ),
                ContinuationBinding {
                    domain: domain.clone(),
                    owner: owner(),
                    revision: view.revision,
                    snapshot_sha256: view.snapshot.sha256().map_err(display)?,
                },
            )
            .await
            .map_err(display)?;
        let live = Arc::new(Live {
            lease,
            stop: self.inner.stop.child_token(),
            done: CancellationToken::new(),
            _capacity: permit,
        });
        // Only publication and task admission share the Host stop gate.
        let _gate = self.inner.gate.lock().unwrap();
        if !epoch.is_open() || cancellation.is_cancelled() {
            live.lease.revoke();
            return Ok(false);
        }
        self.inner
            .owners
            .lock()
            .unwrap()
            .insert(caller.session_id().clone(), live.clone());
        self.inner
            .failures
            .lock()
            .unwrap()
            .remove(caller.session_id());
        let service = self.clone();
        self.inner.tasks.spawn(async move {
            let outcome = tokio::select! { biased; () = live.stop.cancelled() => Ok(()), result = std::panic::AssertUnwindSafe(service.drive(&live)).catch_unwind() => result.unwrap_or_else(|_| Err("Schedule driver panicked".into())) };
            let cleanup = tokio::time::timeout(Duration::from_secs(20), service.finish(&live)).await
                .unwrap_or_else(|_| Err("Schedule reservation cleanup timed out".into()));
            if let Err(problem) = outcome.and(cleanup) {
                service.record_failure(live.lease.session_id(), &problem);
            }
            let done = live.done.clone();
            drop(live);
            done.cancel();
        });
        Ok(true)
    }
}
fn display(error: impl std::fmt::Display) -> String {
    error.to_string()
}
/// Publishes one native Host-generation controller with a production UTC clock.
#[derive(Clone, Debug)]
pub struct ScheduleControllerFactory {
    clock: Arc<dyn ScheduleClock>,
}
impl Default for ScheduleControllerFactory {
    fn default() -> Self {
        Self {
            clock: Arc::new(UtcClock),
        }
    }
}
impl ScheduleControllerFactory {
    /// Selects the exact UTC source and timer implementation for this Host.
    pub fn with_clock(clock: Arc<dyn ScheduleClock>) -> Self {
        Self { clock }
    }
}
#[async_trait]
impl PluginFactory for ScheduleControllerFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "Schedule controller configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<TurnServiceContract>()
            .requiring_local::<TurnExecutionContract>()
            .requiring_local::<SessionCommandsContract>()
            .requiring_local::<SessionContinuationsContract>()
            .requiring_local::<SessionProjectionsContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let service = Arc::new(ScheduleService::new(
            plan.local::<TurnServiceContract>()?,
            plan.local::<TurnExecutionContract>()?,
            plan.local::<SessionCommandsContract>()?,
            plan.local::<SessionContinuationsContract>()?,
            plan.local::<SessionProjectionsContract>()?,
            self.clock.clone(),
        ));
        let supply = plan
            .context()
            .provide_local::<ScheduleControllerContract>(service.clone())?;
        plan.defer(
            "stop Schedule controllers",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    service.stop().await
                })
            }),
        )
    }
}

async fn retry_contention<T, F, Fut>(mut operation: F) -> Result<T, TurnError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, TurnError>>,
{
    for retry in 0..=6 {
        match operation().await {
            Err(error)
                if error.is_continuation_contention()
                    && error != TurnError::SessionBusy
                    && retry < 6 =>
            {
                tokio::time::sleep(Duration::from_millis(10 << retry)).await;
            }
            result => return result,
        }
    }
    unreachable!("last attempt always returns")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn contention_retries_fresh_operation_and_is_bounded() {
        let calls = std::cell::Cell::new(0);
        let result = retry_contention(|| {
            calls.set(calls.get() + 1);
            std::future::ready(if calls.get() < 3 {
                Err(TurnError::Capacity)
            } else {
                Ok(42)
            })
        })
        .await;
        assert_eq!(result, Ok(42));
        assert_eq!(calls.get(), 3);
        calls.set(0);
        let start = tokio::time::Instant::now();
        let result: Result<(), _> = retry_contention(|| {
            calls.set(calls.get() + 1);
            std::future::ready(Err(TurnError::Capacity))
        })
        .await;
        assert_eq!(result, Err(TurnError::Capacity));
        assert_eq!(calls.get(), 7);
        assert_eq!(start.elapsed(), Duration::from_millis(630));
    }
    #[tokio::test]
    async fn uncertain_commit_and_idle_wait_are_never_replayed() {
        for error in [
            TurnError::DomainOutcomeUnknown {
                request_id: "uncertain".into(),
            },
            TurnError::SessionBusy,
        ] {
            let calls = std::cell::Cell::new(0);
            let result: Result<(), _> = retry_contention(|| {
                calls.set(calls.get() + 1);
                std::future::ready(Err(error.clone()))
            })
            .await;
            assert_eq!(result, Err(error));
            assert_eq!(calls.get(), 1);
        }
    }
}
