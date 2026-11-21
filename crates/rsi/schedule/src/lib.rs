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
    CommandArguments, CommandRevision, ContinuationInput, ContributionId, DomainIdentity,
    DomainMutationSource, DomainStateView, MessageId, SessionCommandInvocation, SessionId,
};
use rsi_agent_store_protocol::{SessionStore, SessionStoreContract, StoreError};
use rsi_agent_turn_protocol::{
    AgentCallerAuthority, ContinuationBinding, ContinuationLease, DomainMutationReceipt,
    MessageState, SessionContinuations, SessionContinuationsContract, SessionProjections,
    SessionProjectionsContract, SubmitSession, TurnError, TurnExecution, TurnExecutionContract,
    TurnService, TurnServiceContract,
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
    context: rsi_meta::Context,
    delivery_holds: tokio::sync::watch::Sender<rsi_agent_schedule::DeliveryHolds>,
    turns: Arc<dyn TurnService>,
    execution: Arc<dyn TurnExecution>,
    store: Arc<dyn SessionStore>,
    continuations: Arc<dyn SessionContinuations>,
    projections: Arc<dyn SessionProjections>,
    clock: Arc<dyn ScheduleClock>,
    epoch: ScheduleEpoch,
    stop: CancellationToken,
    gate: Mutex<()>,
    session_gates: Mutex<BTreeMap<SessionId, Weak<AsyncMutex<()>>>>,
    arming: Arc<Semaphore>,
    failures: Mutex<BTreeMap<SessionId, String>>,
    calendar: Mutex<Calendar>,
    changed: tokio::sync::Notify,
    capacity: Arc<Semaphore>,
    tasks: TaskTracker,
}
#[derive(Debug, Default)]
struct Calendar {
    pending: BTreeMap<SessionId, u64>,
    running: BTreeMap<SessionId, Arc<Live>>,
}
#[derive(Debug)]
struct Live {
    lease: ContinuationLease,
    stop: CancellationToken,
    done: CancellationToken,
    _capacity: tokio::sync::OwnedSemaphorePermit,
}
impl ScheduleService {
    /// Builds a controller with an injectable UTC clock. Call `start` once
    /// its dependencies and cleanup have been registered.
    pub fn new(
        context: rsi_meta::Context,
        turns: Arc<dyn TurnService>,
        execution: Arc<dyn TurnExecution>,
        store: Arc<dyn SessionStore>,
        continuations: Arc<dyn SessionContinuations>,
        projections: Arc<dyn SessionProjections>,
        clock: Arc<dyn ScheduleClock>,
    ) -> Self {
        let stop = CancellationToken::new();
        Self {
            inner: Arc::new(Inner {
                context,
                delivery_holds: tokio::sync::watch::channel(BTreeMap::new()).0,
                turns,
                execution,
                store,
                continuations,
                projections,
                clock,
                epoch: ScheduleEpoch::new(stop.clone()),
                stop,
                gate: Mutex::new(()),
                session_gates: Mutex::new(BTreeMap::new()),
                arming: Arc::new(Semaphore::new(64)),
                failures: Mutex::new(BTreeMap::new()),
                calendar: Mutex::default(),
                changed: tokio::sync::Notify::new(),
                capacity: Arc::new(Semaphore::new(64)),
                tasks: TaskTracker::new(),
            }),
        }
    }
    /// Runs the generation's shared timer until shutdown.
    pub fn start(self, startup: Option<tokio::sync::watch::Receiver<bool>>) {
        let dispatcher = self.clone();
        self.inner.tasks.spawn(async move {
            let run = async {
                if let Some(mut ready) = startup
                    && ready.wait_for(|started| *started).await.is_err()
                {
                    return;
                }
                dispatcher.dispatch(&dispatcher.inner.context).await;
            };
            tokio::select! { biased; () = dispatcher.inner.stop.cancelled() => {}, () = run => {} }
        });
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
            let mut calendar = self.inner.calendar.lock().unwrap();
            calendar.pending.clear();
            for live in calendar.running.values() {
                live.lease.revoke();
                live.stop.cancel();
            }
            self.inner.tasks.close();
        }
        tokio::time::timeout(Duration::from_secs(30), self.inner.tasks.wait())
            .await
            .map_err(|_| "Schedule cleanup timed out")?;
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
    ) -> Result<(DomainStateView, ScheduleState, u64), TurnError> {
        let (control_seq, view) = self
            .inner
            .turns
            .domain_state(session, SCHEDULE_DOMAIN)
            .await?
            .ok_or_else(|| TurnError::Invalid("Schedule domain unavailable".into()))?;
        let state = ScheduleState::from_snapshot(&view.snapshot).map_err(TurnError::Invalid)?;
        Ok((view, state, control_seq))
    }
    async fn command(
        &self,
        live: &Live,
        input: &ContinuationInput,
        reserve: Option<(u64, u64)>,
    ) -> Result<(), TurnError> {
        let session = live.lease.session_id();
        let (command, operation, arguments) = if let Some((now_ms, _)) = reserve {
            (
                SCHEDULE_RESERVE,
                "reserve",
                serde_json::to_value(ReserveSchedule { now_ms }).expect("bounded UTC value"),
            )
        } else {
            (
                SCHEDULE_SETTLE,
                "settle",
                serde_json::to_value(SettleSchedule {
                    message_id: input.message_id.clone(),
                })
                .expect("bounded message identity"),
            )
        };
        let command =
            ContributionId::new(command).map_err(|e| TurnError::Invalid(e.to_string()))?;
        let request_id = request_id(input.round, operation).map_err(TurnError::Invalid)?;
        let arguments =
            CommandArguments::new(arguments).map_err(|e| TurnError::Invalid(e.to_string()))?;
        retry_contention(|| async {
            let prepared = self.inner.turns.prepare_resume(session).await?;
            let control_seq = if let Some((_, cut)) = reserve {
                cut
            } else {
                self.inner
                    .store
                    .read_watermarks(session)
                    .await
                    .map_err(|e| match e {
                        StoreError::ReadCapacity | StoreError::ValidationBusy => {
                            TurnError::Capacity
                        }
                        error => TurnError::Store(error.to_string()),
                    })?
                    .durable_control_seq
            };
            self.inner
                .continuations
                .execute(
                    &live.lease,
                    prepared,
                    SessionCommandInvocation {
                        command: command.clone(),
                        request_id: request_id.clone(),
                        expected_revision: CommandRevision::Durable { control_seq },
                        arguments: arguments.clone(),
                    },
                    reserve.map(|_| input.clone()),
                )
                .await?;
            Ok(())
        })
        .await
    }
    async fn drive(
        &self,
        live: &Live,
        mut state: ScheduleState,
        mut control_seq: u64,
        context: &rsi_meta::Context,
    ) -> Result<Option<u64>, String> {
        loop {
            if live.stop.is_cancelled() {
                return Ok(None);
            }
            if let Some(reservation) = state.reservation.as_ref().filter(|r| !r.settled) {
                self.wait_terminal(live, &reservation.input.message_id, live.stop.clone())
                    .await?;
                tokio::select! {
                    () = live.stop.cancelled() => return Err("Schedule wait cancelled".into()),
                    result = self.inner.turns.wait_idle(live.lease.session_id()) => result.map_err(display)?,
                }
                self.command(live, &reservation.input, None)
                    .await
                    .map_err(display)?;
            } else {
                let Some(due) = state.next_due() else {
                    return Ok(None);
                };
                let now = self.inner.clock.now_ms();
                if now < due
                    || context
                        .lookup_local::<rsi_navigation::NavigationContract>()
                        .is_some_and(|owner| {
                            owner.archived_sessions().contains(live.lease.session_id())
                        })
                {
                    return Ok(Some(due));
                }
                let input = state.reserve(now)?;
                match self.command(live, &input, Some((now, control_seq))).await {
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
            (_, state, control_seq) = retry_contention(|| self.snapshot(live.lease.session_id()))
                .await
                .map_err(display)?;
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
            if !self
                .delivery_active(live.lease.session_id(), message)
                .await?
            {
                break;
            }
            tokio::select! {
                () = stop.cancelled() => return Err("Schedule wait cancelled".into()),
                next = changes.next() => if next.is_none() { return Err("Schedule observation ended".into()); },
            }
        }
        Ok(())
    }
    async fn delivery_active(
        &self,
        session: &SessionId,
        message: &MessageId,
    ) -> Result<bool, String> {
        let receipt = retry_contention(|| self.inner.turns.message_status(session, message))
            .await
            .map_err(display)?;
        match receipt.state {
            MessageState::Pending => Ok(true),
            MessageState::Discarded { .. } => Ok(false),
            MessageState::Claimed { turn_id, .. } => {
                Ok(
                    retry_contention(|| self.inner.turns.outcome(session, &turn_id))
                        .await
                        .map_err(display)?
                        .is_none(),
                )
            }
        }
    }
    async fn finish(&self, live: &Live) -> Result<(), String> {
        live.lease.revoke();
        let (_, state, _) = retry_contention(|| self.snapshot(live.lease.session_id()))
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
            self.command(live, &reservation.input, None)
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
    fn delivery_holds(&self) -> tokio::sync::watch::Receiver<rsi_agent_schedule::DeliveryHolds> {
        self.inner.delivery_holds.subscribe()
    }

    async fn check_operator_run(
        &self,
        session: &SessionId,
        task: &rsi_agent_session_protocol::DomainRequestId,
        previous: Option<&MessageId>,
    ) -> Result<(), String> {
        if self.inner.stop.is_cancelled() {
            return Err("Schedule runtime is stopping".into());
        }
        if self
            .inner
            .context
            .lookup_local::<rsi_navigation::NavigationContract>()
            .is_some_and(|nav| nav.archived_sessions().contains(session))
        {
            return Err("The linked session is archived".into());
        }
        if self
            .inner
            .delivery_holds
            .borrow()
            .get(session)
            .is_some_and(|tasks| tasks.contains(task))
        {
            return Err("reminder execution is still active".into());
        }
        if let Some(message) = previous
            && self.delivery_active(session, message).await?
        {
            self.hold_delivery(session, task);
            return Err("reminder execution is still active".into());
        }
        Ok(())
    }

    fn hold_delivery(
        &self,
        session: &SessionId,
        task: &rsi_agent_session_protocol::DomainRequestId,
    ) {
        let _gate = self.inner.gate.lock().unwrap();
        if self.inner.stop.is_cancelled() {
            return;
        }
        let mut first = false;
        self.inner.delivery_holds.send_if_modified(|sessions| {
            let tasks = sessions.entry(session.clone()).or_default();
            first = tasks.is_empty();
            tasks.insert(task.clone())
        });
        if !first {
            return;
        }
        let service = self.clone();
        let session = session.clone();
        self.inner.tasks.spawn(async move {
            loop {
                let count = service.inner.delivery_holds.borrow()[&session].len();
                tokio::select! { biased;
                    () = service.inner.stop.cancelled() => return,
                    result = service.inner.turns.wait_idle(&session) => {
                        if let Err(error) = result { service.record_failure(&session, &display(error)); }
                    }
                }
                // A second task may have published after this idle observation.
                if service.inner.delivery_holds.send_if_modified(|sessions| {
                    if sessions[&session].len() != count { return false; }
                    sessions.remove(&session);
                    true
                }) { break; }
            }
        });
    }
    async fn refresh_pending(&self, session: &SessionId) {
        let gate = self.session_gate(session);
        let _session = gate.lock().await;
        if self
            .inner
            .calendar
            .lock()
            .unwrap()
            .pending
            .contains_key(session)
        {
            match retry_contention(|| self.snapshot(session)).await {
                Ok((_, state, _)) => {
                    self.schedule(session, &state);
                }
                Err(error) => self.record_failure(session, &display(error)),
            }
        }
    }

    fn epoch(&self) -> ScheduleEpoch {
        self.inner.epoch.clone()
    }
    fn clock(&self) -> Arc<dyn ScheduleClock> {
        self.inner.clock.clone()
    }
    fn armed(&self, session: &SessionId) -> bool {
        let calendar = self.inner.calendar.lock().unwrap();
        calendar.pending.contains_key(session)
            || calendar
                .running
                .get(session)
                .is_some_and(|live| live.lease.is_armed())
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
        let gate = self.session_gate(session);
        let _session = gate.lock().await;
        let live = {
            let mut calendar = self.inner.calendar.lock().unwrap();
            calendar.pending.remove(session);
            let live = calendar.running.get(session).cloned();
            if let Some(live) = &live {
                live.lease.revoke();
                live.stop.cancel();
            }
            live
        };
        self.inner.changed.notify_one();
        if let Some(live) = live {
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
        let gate = self.session_gate(caller.session_id());
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
        let (view, state, _) = self.snapshot(caller.session_id()).await.map_err(display)?;
        if receipt.commit().updates().len() != 1
            || view.snapshot.identity() != domain
            || receipt.commit().updates()[0].revision() != view.revision
            || receipt.commit().updates()[0].snapshot() != &view.snapshot
        {
            return Ok(false);
        }
        let _gate = self.inner.gate.lock().unwrap();
        if !epoch.is_open() || cancellation.is_cancelled() {
            return Ok(false);
        }
        let session = caller.session_id();
        self.inner.failures.lock().unwrap().remove(session);
        Ok(self.schedule(session, &state))
    }

    fn session_gate(&self, session: &SessionId) -> Arc<AsyncMutex<()>> {
        let mut gates = self.inner.session_gates.lock().unwrap();
        gates.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = gates.get(session).and_then(Weak::upgrade) {
            return gate;
        }
        let gate = Arc::new(AsyncMutex::new(()));
        gates.insert(session.clone(), Arc::downgrade(&gate));
        gate
    }

    fn schedule(&self, session: &SessionId, state: &ScheduleState) -> bool {
        let at = if state.reservation.as_ref().is_some_and(|r| !r.settled) {
            Some(0)
        } else {
            state.next_due()
        };
        let mut calendar = self.inner.calendar.lock().unwrap();
        if let Some(at) = at {
            calendar.pending.insert(session.clone(), at);
        } else {
            calendar.pending.remove(session);
        }
        self.inner.changed.notify_one();
        at.is_some()
    }

    async fn dispatch(&self, context: &rsi_meta::Context) {
        loop {
            if self.inner.calendar.lock().unwrap().pending.is_empty() {
                self.inner.changed.notified().await;
                continue;
            }
            let (next, mut archives_changed) = {
                let navigation = context.lookup_local::<rsi_navigation::NavigationContract>();
                let changes = navigation.as_ref().map(|owner| owner.archive_changes());
                let archived = navigation.as_ref().map(|owner| owner.archived_sessions());
                let calendar = self.inner.calendar.lock().unwrap();
                let next = calendar
                    .pending
                    .iter()
                    .filter(|(id, _)| archived.as_ref().is_none_or(|rows| !rows.contains(id)))
                    .min_by_key(|(_, at)| **at)
                    .map(|(session, at)| (session.clone(), *at));
                (next, changes)
            };
            let deadline = next.as_ref().map(|(_, at)| *at);
            let Some((session, at)) = next.filter(|(_, at)| *at <= self.inner.clock.now_ms())
            else {
                tokio::select! {
                    () = self.inner.changed.notified() => {},
                    () = async { self.inner.clock.wait_until(deadline.expect("pending deadline"), self.inner.stop.clone()).await }, if deadline.is_some() => {},
                    _ = async { archives_changed.as_mut().expect("navigation available").changed().await }, if archives_changed.is_some() => {},
                }
                continue;
            };
            let Ok(permit) = self.inner.capacity.clone().acquire_owned().await else {
                return;
            };
            let gate = self.session_gate(&session);
            let _session = gate.lock().await;
            if self.inner.calendar.lock().unwrap().pending.get(&session) != Some(&at) {
                continue;
            }
            if let Err(problem) = self.start_due(&session, permit, context).await {
                self.inner.calendar.lock().unwrap().pending.remove(&session);
                self.record_failure(&session, &problem);
            }
        }
    }

    async fn start_due(
        &self,
        session: &SessionId,
        permit: tokio::sync::OwnedSemaphorePermit,
        context: &rsi_meta::Context,
    ) -> Result<(), String> {
        if context
            .lookup_local::<rsi_navigation::NavigationContract>()
            .is_some_and(|owner| owner.archived_sessions().contains(session))
        {
            return Ok(());
        }
        let (view, state, control_seq) = retry_contention(|| self.snapshot(session))
            .await
            .map_err(display)?;
        if state.reservation.as_ref().is_none_or(|r| r.settled)
            && state
                .next_due()
                .is_none_or(|at| at > self.inner.clock.now_ms())
        {
            self.schedule(session, &state);
            return Ok(());
        }
        let binding = ContinuationBinding {
            domain: view.snapshot.identity().clone(),
            owner: owner(),
            revision: view.revision,
            snapshot_sha256: view.snapshot.sha256().map_err(display)?,
        };
        let recovery = state.reservation.as_ref().is_some_and(|r| !r.settled);
        let lease = retry_contention(|| async {
            let prepared = SubmitSession::Resume(self.inner.turns.prepare_resume(session).await?);
            if recovery {
                self.inner
                    .continuations
                    .arm_for_recovery(prepared, binding.clone())
                    .await
            } else {
                self.inner
                    .continuations
                    .arm(prepared, binding.clone())
                    .await
            }
        })
        .await;
        let lease = match lease {
            Ok(lease) => lease,
            Err(TurnError::ContinuationDisarmed) => {
                let (current, state, _) = retry_contention(|| self.snapshot(session))
                    .await
                    .map_err(display)?;
                if current.revision == view.revision {
                    return Err(display(TurnError::ContinuationDisarmed));
                }
                self.schedule(session, &state);
                return Ok(());
            }
            Err(error) => return Err(display(error)),
        };
        let live = Arc::new(Live {
            lease,
            stop: self.inner.stop.child_token(),
            done: CancellationToken::new(),
            _capacity: permit,
        });
        let _gate = self.inner.gate.lock().unwrap();
        if self.inner.stop.is_cancelled() {
            live.lease.revoke();
            return Ok(());
        }
        {
            let mut calendar = self.inner.calendar.lock().unwrap();
            calendar.pending.remove(session);
            calendar.running.insert(session.clone(), live.clone());
        }
        let service = self.clone();
        let context = context.clone();
        self.inner.tasks.spawn(async move {
            let result = tokio::select! { biased;
                () = live.stop.cancelled() => Ok(None),
                result = std::panic::AssertUnwindSafe(service.drive(&live, state, control_seq, &context)).catch_unwind() =>
                    result.unwrap_or_else(|_| Err("Schedule driver panicked".into())),
            };
            let cleanup = if result.is_err() || live.stop.is_cancelled() {
                tokio::time::timeout(Duration::from_secs(20), service.finish(&live))
                    .await
                    .unwrap_or_else(|_| Err("Schedule reservation cleanup timed out".into()))
            } else {
                live.lease.revoke();
                Ok(())
            };
            let session = live.lease.session_id();
            let mut calendar = service.inner.calendar.lock().unwrap();
            calendar.running.remove(session);
            match result.and_then(|next| cleanup.map(|()| next)) {
                Ok(Some(at)) if !live.stop.is_cancelled() => {
                    calendar.pending.insert(session.clone(), at);
                }
                Err(problem) => service.record_failure(session, &problem),
                _ => {}
            }
            live.done.cancel();
            service.inner.changed.notify_one();
        });
        Ok(())
    }

    async fn restore(&self) -> Result<(), String> {
        let mut after = None;
        loop {
            let page = self
                .inner
                .store
                .list_edited_domain_sessions(SCHEDULE_DOMAIN, after.as_ref(), 64)
                .await
                .map_err(display)?;
            page.validate().map_err(display)?;
            for session in page.sessions {
                let selected =
                    retry_contention(|| self.inner.turns.domain_state(&session, SCHEDULE_DOMAIN))
                        .await;
                match selected.map_err(display).and_then(|row| {
                    row.map(|(_, row)| ScheduleState::from_snapshot(&row.snapshot))
                        .transpose()
                }) {
                    Ok(Some(state)) => {
                        for reminder in &state.reminders {
                            if let Some(last) = &reminder.last_delivery
                                && !state
                                    .reservation
                                    .as_ref()
                                    .is_some_and(|r| r.input.message_id == last.message_id)
                            {
                                match self.delivery_active(&session, &last.message_id).await {
                                    Ok(true) => self.hold_delivery(&session, &reminder.id),
                                    Ok(false) => {}
                                    Err(problem) => self.record_failure(&session, &problem),
                                }
                            }
                        }
                        self.schedule(&session, &state);
                    }
                    Ok(None) => {}
                    Err(problem) => self.record_failure(&session, &problem),
                }
                after = Some(session);
            }
            if !page.has_more {
                return Ok(());
            }
        }
    }
}
fn display(error: impl std::fmt::Display) -> String {
    error.to_string()
}
/// Publishes one native Host-generation controller with a production UTC clock.
#[derive(Clone, Debug)]
pub struct ScheduleControllerFactory {
    clock: Arc<dyn ScheduleClock>,
    startup: Option<tokio::sync::watch::Receiver<bool>>,
}
impl Default for ScheduleControllerFactory {
    fn default() -> Self {
        Self {
            clock: Arc::new(UtcClock),
            startup: None,
        }
    }
}
impl ScheduleControllerFactory {
    /// Selects the exact UTC source and timer implementation for this Host.
    pub fn with_clock(clock: Arc<dyn ScheduleClock>) -> Self {
        Self {
            clock,
            startup: None,
        }
    }
    /// Waits for a composed Host's startup owners before delivering retained reminders.
    #[must_use]
    pub fn with_startup(mut self, ready: Option<tokio::sync::watch::Receiver<bool>>) -> Self {
        self.startup = ready;
        self
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
            .requiring_local::<SessionStoreContract>()
            .requiring_local::<TurnServiceContract>()
            .requiring_local::<TurnExecutionContract>()
            .requiring_local::<SessionContinuationsContract>()
            .requiring_local::<SessionProjectionsContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let service = ScheduleService::new(
            plan.context().clone(),
            plan.local::<TurnServiceContract>()?,
            plan.local::<TurnExecutionContract>()?,
            plan.local::<SessionStoreContract>()?,
            plan.local::<SessionContinuationsContract>()?,
            plan.local::<SessionProjectionsContract>()?,
            self.clock.clone(),
        );
        service.restore().await.map_err(MetaError::InvalidInput)?;
        let supply = plan
            .context()
            .provide_local::<ScheduleControllerContract>(Arc::new(service.clone()))?;
        let cleanup = service.clone();
        plan.defer(
            "stop Schedule controllers",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    cleanup.stop().await
                })
            }),
        )?;
        service.start(self.startup.clone());
        Ok(())
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
