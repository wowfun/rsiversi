#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;

struct ReconciliationSlot {
    _usage: ResourceReservation,
    _admission: tokio::sync::OwnedSemaphorePermit,
}

struct PausedReconciliation {
    inner: Arc<RuntimeInner>,
}

struct NestedIntentClaim {
    runtime: Runtime,
    id: FiberId,
    completed: bool,
}

struct ReconciliationRun {
    revision: u64,
    saturated_completion_prefix: Option<usize>,
}

impl ReconciliationFrontier {
    const TOMBSTONE_HEADROOM: usize = 64;

    fn enqueue_ready(&mut self, id: FiberId) {
        if self.ready.contains_key(&id) {
            return;
        }
        let token = self.next_ready_token();
        self.ready.insert(id, token);
        self.ready_order.push_back((id, token));
    }

    fn remove_ready(&mut self, id: FiberId) -> bool {
        let removed = self.ready.remove(&id).is_some();
        if removed
            && self.ready_order.len()
                > self
                    .ready
                    .len()
                    .saturating_mul(2)
                    .saturating_add(Self::TOMBSTONE_HEADROOM)
        {
            self.compact_ready_order();
        }
        removed
    }

    fn compact_ready_order(&mut self) {
        self.ready_order
            .retain(|(id, token)| self.ready.get(id) == Some(token));
    }

    fn next_ready_token(&mut self) -> u64 {
        if self.next_ready_token == u64::MAX {
            self.compact_ready_order();
            let mut compacted = VecDeque::with_capacity(self.ready.len());
            for (id, token) in std::mem::take(&mut self.ready_order) {
                if self.ready.get(&id) != Some(&token) {
                    continue;
                }
                let compacted_token = u64::try_from(compacted.len())
                    .expect("a FiberId-bounded frontier fits u64")
                    + 1;
                *self
                    .ready
                    .get_mut(&id)
                    .expect("a live ready entry remains indexed") = compacted_token;
                compacted.push_back((id, compacted_token));
            }
            self.ready_order = compacted;
            self.next_ready_token =
                u64::try_from(self.ready.len()).expect("a FiberId-bounded frontier fits u64");
        }
        self.next_ready_token += 1;
        self.next_ready_token
    }

    fn enqueue(&mut self, id: FiberId) {
        if self.active.contains(&id) {
            debug_assert!(!self.ready.contains_key(&id));
            self.rerun.insert(id);
        } else {
            debug_assert!(!self.rerun.contains(&id));
            self.enqueue_ready(id);
        }
    }

    fn claim(&mut self, id: FiberId) -> bool {
        if !self.remove_ready(id) {
            return false;
        }
        let inserted = self.active.insert(id);
        debug_assert!(inserted);
        debug_assert!(!self.rerun.contains(&id));
        true
    }

    fn take(&mut self, active_limit: usize) -> Option<FiberId> {
        if self.active.len() >= active_limit {
            return None;
        }
        let id = loop {
            let (id, token) = self.ready_order.pop_front()?;
            if self.ready.get(&id) == Some(&token) {
                self.ready.remove(&id);
                break id;
            }
        };
        let inserted = self.active.insert(id);
        debug_assert!(inserted);
        debug_assert!(!self.rerun.contains(&id));
        Some(id)
    }

    fn finish(&mut self, id: FiberId, fiber_exists: bool) {
        let removed = self.active.remove(&id);
        debug_assert!(removed);
        let requested_rerun = self.rerun.remove(&id);
        if requested_rerun && fiber_exists {
            self.enqueue_ready(id);
        }
        debug_assert!(!self.active.contains(&id));
        debug_assert!(!self.rerun.contains(&id));
    }

    fn abandon(&mut self, id: FiberId, fiber_exists: bool) {
        let removed = self.active.remove(&id);
        debug_assert!(removed);
        self.rerun.remove(&id);
        if fiber_exists {
            self.enqueue_ready(id);
        }
        debug_assert!(!self.active.contains(&id));
        debug_assert!(!self.rerun.contains(&id));
    }

    pub(super) fn remove_queued(&mut self, id: FiberId) {
        self.remove_ready(id);
        self.rerun.remove(&id);
    }

    fn has_queued(&self) -> bool {
        !self.ready.is_empty() || !self.rerun.is_empty()
    }

    fn is_idle(&self) -> bool {
        self.ready.is_empty() && self.active.is_empty() && self.rerun.is_empty()
    }

    #[cfg(test)]
    fn invariants_hold(&self) -> bool {
        let ordered = self
            .ready_order
            .iter()
            .filter(|(id, token)| self.ready.get(id) == Some(token))
            .map(|(id, _)| *id)
            .collect::<BTreeSet<_>>();
        ordered.len() == self.ready.len()
            && ordered.iter().all(|id| self.ready.contains_key(id))
            && self.ready.keys().all(|id| !self.active.contains(id))
            && self.ready.keys().all(|id| !self.rerun.contains(id))
            && self.rerun.is_subset(&self.active)
    }
}

impl NestedIntentClaim {
    fn new(runtime: &Runtime, id: FiberId) -> Self {
        Self {
            runtime: runtime.clone(),
            id,
            completed: false,
        }
    }

    fn finish(mut self) {
        self.completed = true;
        self.runtime.finish_scheduled_intent(self.id);
    }
}

impl Drop for NestedIntentClaim {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let mut state = self
            .runtime
            .inner
            .state
            .lock()
            .expect("runtime state poisoned");
        let fiber_exists = state.fibers.contains_key(&self.id);
        state.reconciliations.abandon(self.id, fiber_exists);
        drop(state);
        self.runtime.inner.scheduler_wakeup.notify_one();
    }
}

impl PausedReconciliation {
    fn new(inner: &Arc<RuntimeInner>) -> Self {
        inner.paused_reconciliations.fetch_add(1, Ordering::AcqRel);
        inner.scheduler_wakeup.notify_one();
        Self {
            inner: Arc::clone(inner),
        }
    }
}

impl Drop for PausedReconciliation {
    fn drop(&mut self) {
        let previous = self
            .inner
            .paused_reconciliations
            .fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0);
        self.inner.scheduler_wakeup.notify_one();
    }
}

tokio::task_local! {
    /// The one scheduler slot held by the current Fiber transition. Nested
    /// apply and dependent barriers temporarily transfer this slot instead of
    /// blocking behind their own parent.
    static RECONCILIATION_SLOT: std::cell::RefCell<Option<ReconciliationSlot>>;
}

impl Fiber {
    fn cancel_loading_activation(&self) {
        let data = self.data.lock().expect("fiber state poisoned");
        if matches!(data.state, FiberState::Loading)
            && let Some(active) = data.active.as_ref()
        {
            active.activation_cancellation.cancel();
        }
    }
}

impl ReconciliationProgress {
    fn request(&self) -> ReconciliationTicket {
        let completion = Arc::new(ReconciliationCompletion::default());
        let mut completions = self
            .completions
            .lock()
            .expect("reconciliation completions poisoned");
        // Settlement takes this same lock before selecting completions. Publish
        // the desired revision only after owning it so settlement can never
        // observe a revision whose completion is not registered yet.
        let revision = self.request_revision();
        completions
            .entry(revision)
            .or_default()
            .push(Arc::downgrade(&completion));
        drop(completions);
        ReconciliationTicket {
            receiver: self.watch.subscribe(),
            completion,
        }
    }

    fn request_revision(&self) -> u64 {
        self.desired
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |revision| {
                Some(revision.saturating_add(1))
            })
            .expect("reconciliation revision update cannot fail")
            .saturating_add(1)
    }

    fn desired(&self) -> u64 {
        self.desired.load(Ordering::Acquire)
    }

    fn settled(&self) -> u64 {
        self.settled.load(Ordering::Acquire)
    }

    fn begin_run(&self) -> ReconciliationRun {
        let completions = self
            .completions
            .lock()
            .expect("reconciliation completions poisoned");
        let revision = self.desired();
        let saturated_completion_prefix = (revision == u64::MAX)
            .then(|| completions.get(&u64::MAX).map_or(0, std::vec::Vec::len));
        ReconciliationRun {
            revision,
            saturated_completion_prefix,
        }
    }

    fn mark_settled(&self, run: &ReconciliationRun, snapshot: &FiberSnapshot) {
        self.settle(run.revision, run.saturated_completion_prefix, snapshot);
    }

    fn settle(
        &self,
        revision: u64,
        saturated_completion_prefix: Option<usize>,
        snapshot: &FiberSnapshot,
    ) {
        let pending = {
            let mut completions = self
                .completions
                .lock()
                .expect("reconciliation completions poisoned");
            if revision == u64::MAX {
                let mut saturated = completions.remove(&u64::MAX).unwrap_or_default();
                let later = saturated_completion_prefix.map(|prefix| {
                    debug_assert!(prefix <= saturated.len());
                    saturated.split_off(prefix.min(saturated.len()))
                });
                let mut pending = std::mem::take(&mut *completions)
                    .into_values()
                    .flatten()
                    .collect::<Vec<_>>();
                pending.append(&mut saturated);
                if let Some(later) = later
                    && !later.is_empty()
                {
                    completions.insert(u64::MAX, later);
                }
                pending
            } else {
                let later = completions.split_off(&(revision + 1));
                std::mem::replace(&mut *completions, later)
                    .into_values()
                    .flatten()
                    .collect()
            }
        };
        for completion in pending {
            if let Some(completion) = completion.upgrade() {
                *completion
                    .snapshot
                    .lock()
                    .expect("reconciliation completion poisoned") = Some(snapshot.clone());
            }
        }
        self.settled.fetch_max(revision, Ordering::AcqRel);
        if revision == u64::MAX {
            // The terminal counter value cannot advance, so notify receivers
            // even when the watch already contains MAX. Their per-ticket
            // completion snapshot is the authority for this new request.
            self.watch.send_replace(revision);
            return;
        }
        self.watch.send_if_modified(|settled| {
            if *settled >= revision {
                false
            } else {
                *settled = revision;
                true
            }
        });
    }

    pub(super) fn finish(&self, snapshot: &FiberSnapshot) {
        // Terminal disposal owns every pending intent, including tickets
        // registered after a reconciliation run began.
        self.settle(self.desired(), None, snapshot);
    }
}

impl ReconciliationTicket {
    pub(super) async fn join(mut self) -> FiberSnapshot {
        loop {
            if let Some(snapshot) = self
                .completion
                .snapshot
                .lock()
                .expect("reconciliation completion poisoned")
                .clone()
            {
                return snapshot;
            }
            if self.receiver.changed().await.is_err() {
                return self
                    .completion
                    .snapshot
                    .lock()
                    .expect("reconciliation completion poisoned")
                    .clone()
                    .expect("a closed reconciliation watch has completed every ticket");
            }
        }
    }
}

impl Drop for PendingApplyOwnership {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.fiber.apply_cancellation.cancel();
        if let Some(inner) = self.runtime.upgrade() {
            let _executor_guard = self.fiber.executor.enter();
            Runtime { inner }.request_disposal(&self.fiber);
        }
    }
}

impl Runtime {
    pub(super) fn request_reconciliation(&self, id: FiberId) -> Option<ReconciliationTicket> {
        let (ticket, executor) = {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            let fiber = state.fibers.get(&id).cloned()?;
            let ticket = fiber.reconciliation.request();
            fiber.cancel_loading_activation();
            state.reconciliations.enqueue(id);
            let should_spawn = if state.reconciliation_worker_running {
                false
            } else {
                state.reconciliation_worker_running = true;
                true
            };
            let executor = should_spawn.then(|| fiber.executor.clone());
            (ticket, executor)
        };
        if let Some(executor) = executor {
            self.spawn_reconciliation_worker(&executor);
        }
        self.inner.scheduler_wakeup.notify_one();
        Some(ticket)
    }

    pub(super) fn request_disposal(&self, fiber: &Arc<Fiber>) {
        if !fiber.disposal.try_start() {
            return;
        }
        let executor = {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            if !state.fibers.contains_key(&fiber.id) {
                return;
            }
            // This shares the state -> Fiber-data transaction used by Loading
            // installation. Disposal either fences a not-yet-installed
            // generation there or observes and cancels its exact token here.
            fiber.disposal_requested.cancel();
            fiber.cancel_loading_activation();
            state.reconciliations.enqueue(fiber.id);
            let should_spawn = if state.reconciliation_worker_running {
                false
            } else {
                state.reconciliation_worker_running = true;
                true
            };
            should_spawn.then(|| fiber.executor.clone())
        };
        if let Some(executor) = executor {
            self.spawn_reconciliation_worker(&executor);
        }
        self.inner.scheduler_wakeup.notify_one();
    }

    /// A transition that synchronously awaits another Fiber cooperatively takes
    /// that already-queued intent through the same scheduler driver. This keeps
    /// a concurrency limit of one live without creating a task per Fiber.
    pub(super) async fn drive_nested_intent(&self, id: FiberId) {
        if RECONCILIATION_SLOT.try_with(|_| ()).is_err() {
            return;
        }
        let claimed = {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            state.reconciliations.claim(id)
        };
        if !claimed {
            return;
        }
        let claim = NestedIntentClaim::new(self, id);
        self.drive_transition(id).await;
        claim.finish();
    }

    /// Runs an operation without retaining the current transition slot, then
    /// reacquires the slot before the enclosing Fiber resumes mutation.
    pub(super) async fn yield_reconciliation_slot<F, T>(&self, operation: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        let permit = RECONCILIATION_SLOT
            .try_with(|slot| slot.borrow_mut().take())
            .ok()
            .flatten();
        let Some(permit) = permit else {
            return operation.await;
        };
        drop(permit);
        let paused = PausedReconciliation::new(&self.inner);
        // Reinstall the slot before an unwind reaches the enclosing
        // reconciliation catch boundary, so rollback cannot bypass admission.
        let output = std::panic::AssertUnwindSafe(operation).catch_unwind().await;
        let permit = self.acquire_reconciliation_slot().await;
        RECONCILIATION_SLOT.with(|slot| {
            let previous = slot.borrow_mut().replace(permit);
            debug_assert!(previous.is_none());
        });
        drop(paused);
        match output {
            Ok(output) => output,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    pub(super) async fn with_reconciliation_slot<F, T>(&self, operation: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        let permit = self.acquire_reconciliation_slot().await;
        RECONCILIATION_SLOT
            .scope(std::cell::RefCell::new(Some(permit)), operation)
            .await
    }

    async fn acquire_reconciliation_slot(&self) -> ReconciliationSlot {
        let admission = Arc::clone(&self.inner.reconciliation_admission)
            .acquire_owned()
            .await
            .expect("the Runtime never closes reconciliation admission");
        let usage = self
            .inner
            .resources
            .reconciliations
            .try_reserve(1)
            .expect("reconciliation semaphore and resource ledger stay synchronized");
        ReconciliationSlot {
            _usage: usage,
            _admission: admission,
        }
    }

    pub(super) fn notify_service_appearances(
        &self,
        services: &[ServiceSlot],
        except: Option<FiberId>,
    ) {
        let executor = {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            let affected = Self::dependent_ids(&state, services, except);
            let mut executor = None;
            for id in affected {
                if let Some(fiber) = state.fibers.get(&id) {
                    executor.get_or_insert_with(|| fiber.executor.clone());
                    fiber.reconciliation.request_revision();
                    state.reconciliations.enqueue(id);
                }
            }
            let should_spawn =
                if !state.reconciliations.has_queued() || state.reconciliation_worker_running {
                    false
                } else {
                    state.reconciliation_worker_running = true;
                    true
                };
            debug_assert!(!should_spawn || executor.is_some());
            should_spawn.then_some(executor).flatten()
        };
        if let Some(executor) = executor {
            self.spawn_reconciliation_worker(&executor);
        }
        self.inner.scheduler_wakeup.notify_one();
    }

    pub(super) fn notify_local_appearances(&self, services: &[LocalSlot], except: Option<FiberId>) {
        let executor = {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            let affected = Self::local_dependent_ids(&state, services, except);
            let mut executor = None;
            for id in affected {
                if let Some(fiber) = state.fibers.get(&id) {
                    executor.get_or_insert_with(|| fiber.executor.clone());
                    fiber.reconciliation.request_revision();
                    state.reconciliations.enqueue(id);
                }
            }
            let should_spawn =
                if !state.reconciliations.has_queued() || state.reconciliation_worker_running {
                    false
                } else {
                    state.reconciliation_worker_running = true;
                    true
                };
            debug_assert!(!should_spawn || executor.is_some());
            should_spawn.then_some(executor).flatten()
        };
        if let Some(executor) = executor {
            self.spawn_reconciliation_worker(&executor);
        }
        self.inner.scheduler_wakeup.notify_one();
    }

    /// Fences every exact dependent in the same Runtime-state transaction that
    /// removes service visibility. The caller must hold `state` across both
    /// operations so a Loading generation cannot publish a withdrawn binding.
    pub(super) fn request_service_withdrawals_locked(
        state: &mut RuntimeState,
        services: &[ServiceSlot],
        except: Option<FiberId>,
    ) -> (
        Vec<(FiberId, ReconciliationTicket)>,
        Option<tokio::runtime::Handle>,
    ) {
        let affected = Self::dependent_ids(state, services, except);
        let mut tickets = Vec::with_capacity(affected.len());
        let mut executor = None;
        for id in affected {
            if let Some(fiber) = state.fibers.get(&id) {
                executor.get_or_insert_with(|| fiber.executor.clone());
                let ticket = fiber.reconciliation.request();
                fiber.cancel_loading_activation();
                state.reconciliations.enqueue(id);
                tickets.push((id, ticket));
            }
        }
        let should_spawn =
            if !state.reconciliations.has_queued() || state.reconciliation_worker_running {
                false
            } else {
                state.reconciliation_worker_running = true;
                true
            };
        debug_assert!(!should_spawn || executor.is_some());
        (tickets, should_spawn.then_some(executor).flatten())
    }

    pub(super) fn request_local_withdrawals_locked(
        state: &mut RuntimeState,
        services: &[LocalSlot],
        except: Option<FiberId>,
    ) -> (
        Vec<(FiberId, ReconciliationTicket)>,
        Option<tokio::runtime::Handle>,
    ) {
        let affected = Self::local_dependent_ids(state, services, except);
        let mut tickets = Vec::with_capacity(affected.len());
        let mut executor = None;
        for id in affected {
            if let Some(fiber) = state.fibers.get(&id) {
                executor.get_or_insert_with(|| fiber.executor.clone());
                let ticket = fiber.reconciliation.request();
                fiber.cancel_loading_activation();
                state.reconciliations.enqueue(id);
                tickets.push((id, ticket));
            }
        }
        let should_spawn =
            if !state.reconciliations.has_queued() || state.reconciliation_worker_running {
                false
            } else {
                state.reconciliation_worker_running = true;
                true
            };
        debug_assert!(!should_spawn || executor.is_some());
        (tickets, should_spawn.then_some(executor).flatten())
    }

    pub(super) fn start_reconciliation_requests(&self, executor: Option<tokio::runtime::Handle>) {
        if let Some(executor) = executor {
            self.spawn_reconciliation_worker(&executor);
        }
        self.inner.scheduler_wakeup.notify_one();
    }

    fn dependent_ids(
        state: &RuntimeState,
        services: &[ServiceSlot],
        except: Option<FiberId>,
    ) -> BTreeSet<FiberId> {
        services
            .iter()
            .filter_map(|service| state.dependents.get(service))
            .flat_map(BTreeSet::iter)
            .copied()
            .filter(|id| Some(*id) != except)
            .collect()
    }

    fn local_dependent_ids(
        state: &RuntimeState,
        services: &[LocalSlot],
        except: Option<FiberId>,
    ) -> BTreeSet<FiberId> {
        services
            .iter()
            .filter_map(|service| state.local_dependents.get(service))
            .flat_map(BTreeSet::iter)
            .copied()
            .filter(|id| Some(*id) != except)
            .collect()
    }

    fn spawn_reconciliation_worker(&self, executor: &tokio::runtime::Handle) {
        let usage = self
            .inner
            .resources
            .scheduler_workers
            .try_reserve(1)
            .expect("one scheduler-running flag owns the single worker reservation");
        let runtime = self.clone();
        executor.spawn(async move { runtime.run_reconciliation_worker(usage).await });
    }

    async fn run_reconciliation_worker(&self, mut worker_usage: ResourceReservation) {
        let concurrency = self
            .inner
            .limits
            .execution
            .maximum_concurrent_reconciliations;
        let mut active =
            futures_util::stream::FuturesUnordered::<BoxFuture<'static, FiberId>>::new();
        loop {
            let active_limit = concurrency
                .saturating_add(self.inner.paused_reconciliations.load(Ordering::Acquire))
                .min(self.inner.limits.topology.maximum_fibers);
            while let Some(id) = self.take_scheduled_intent(active_limit) {
                let runtime = self.clone();
                active.push(Box::pin(async move {
                    let transition = contain_panic_result(
                        std::panic::AssertUnwindSafe(runtime.drive_transition(id))
                            .catch_unwind()
                            .await,
                    );
                    if transition.is_err() {
                        runtime.mark_terminal_owned(format!(
                            "runtime-owned Fiber {} transition escaped containment",
                            id.0
                        ));
                    }
                    id
                }));
            }
            if active.is_empty() {
                let should_stop = {
                    let mut state = self.inner.state.lock().expect("runtime state poisoned");
                    if state.reconciliations.has_queued() {
                        false
                    } else {
                        state.reconciliation_worker_running = false;
                        // State serialization keeps the running flag and the
                        // one-worker ledger at one linearization point.
                        worker_usage.shrink_to(0);
                        true
                    }
                };
                if should_stop {
                    self.inner.scheduler_idle.notify_waiters();
                    return;
                }
                self.inner.scheduler_wakeup.notified().await;
                continue;
            }

            let wakeup = self.inner.scheduler_wakeup.notified();
            tokio::pin!(wakeup);
            wakeup.as_mut().enable();
            let completed = tokio::select! {
                completed = active.next() => completed,
                () = wakeup.as_mut() => None,
            };
            if let Some(id) = completed {
                self.finish_scheduled_intent(id);
            }
        }
    }

    fn take_scheduled_intent(&self, active_limit: usize) -> Option<FiberId> {
        let mut state = self.inner.state.lock().expect("runtime state poisoned");
        state.reconciliations.take(active_limit)
    }

    fn finish_scheduled_intent(&self, id: FiberId) {
        let mut state = self.inner.state.lock().expect("runtime state poisoned");
        let fiber_exists = state.fibers.contains_key(&id);
        state.reconciliations.finish(id, fiber_exists);
        drop(state);
        self.inner.scheduler_wakeup.notify_one();
    }

    pub(super) async fn wait_scheduler_idle(&self) {
        loop {
            let notified = self.inner.scheduler_idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let idle = {
                let state = self.inner.state.lock().expect("runtime state poisoned");
                !state.reconciliation_worker_running && state.reconciliations.is_idle()
            };
            if idle {
                return;
            }
            notified.as_mut().await;
        }
    }

    async fn drive_transition(&self, id: FiberId) {
        let Some(fiber) = ({
            let state = self.inner.state.lock().expect("runtime state poisoned");
            state.fibers.get(&id).cloned()
        }) else {
            return;
        };
        // Waiting duplicates do not consume the global scheduler budget. The
        // transition mutex is the authoritative one-active-transition fence.
        let _transition = fiber.transition.lock().await;
        if fiber.disposal_requested.is_cancelled() {
            self.run_scheduled_disposal(Arc::clone(&fiber)).await;
            let snapshot = fiber.watch.borrow().clone();
            fiber.reconciliation.finish(&snapshot);
            return;
        }
        let run = fiber.reconciliation.begin_run();
        let revision = run.revision;
        // At saturation, a queued intent still represents new work even
        // though the counter cannot advance. The run-start completion prefix
        // prevents work registered during this transition from receiving its
        // snapshot before the queued rerun.
        if revision != u64::MAX && fiber.reconciliation.settled() >= revision {
            return;
        }
        self.with_reconciliation_slot(self.reconcile_fiber(Arc::clone(&fiber)))
            .await;
        fiber.reconciliation.mark_settled(&run, &fiber.snapshot());
    }

    pub(super) async fn join_reconciliation_requests(
        &self,
        tickets: Vec<(FiberId, ReconciliationTicket)>,
    ) {
        self.yield_reconciliation_slot(async {
            futures_util::stream::iter(tickets)
                .for_each_concurrent(
                    self.inner
                        .limits
                        .execution
                        .maximum_concurrent_reconciliations,
                    |(id, ticket)| async move {
                        self.drive_nested_intent(id).await;
                        ticket.join().await;
                    },
                )
                .await;
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PreparedActivation, Requirement};

    #[test]
    fn active_rerequests_stay_out_of_the_ready_frontier() {
        const ACTIVE: usize = 4_096;
        let active_id_limit = u64::try_from(ACTIVE).expect("test size fits a Fiber ID");
        let mut frontier = ReconciliationFrontier::default();
        for raw in 1..=active_id_limit {
            frontier.enqueue(FiberId(raw));
        }
        for raw in 1..=active_id_limit {
            assert_eq!(frontier.take(ACTIVE), Some(FiberId(raw)));
        }
        assert!(frontier.ready.is_empty());
        assert_eq!(frontier.active.len(), ACTIVE);

        for raw in 1..=active_id_limit {
            frontier.enqueue(FiberId(raw));
            frontier.enqueue(FiberId(raw));
        }
        assert!(frontier.ready.is_empty());
        assert_eq!(frontier.rerun.len(), ACTIVE);
        assert!(frontier.invariants_hold());
        assert_eq!(frontier.take(ACTIVE + 1), None);

        let completed = FiberId(active_id_limit);
        frontier.finish(completed, true);
        assert_eq!(frontier.ready.len(), 1);
        assert!(frontier.ready.contains_key(&completed));
        assert!(!frontier.rerun.contains(&completed));
        assert!(frontier.invariants_hold());
        assert_eq!(frontier.take(ACTIVE), Some(completed));
    }

    #[test]
    fn rerun_does_not_overtake_an_already_ready_fiber() {
        let repeatedly_requested = FiberId(1);
        let waiting = FiberId(2);
        let mut frontier = ReconciliationFrontier::default();

        frontier.enqueue(repeatedly_requested);
        assert_eq!(frontier.take(1), Some(repeatedly_requested));
        frontier.enqueue(waiting);
        frontier.enqueue(repeatedly_requested);
        frontier.finish(repeatedly_requested, true);

        assert_eq!(
            frontier.take(1),
            Some(waiting),
            "a rerun must join the tail instead of starving ready work"
        );
    }

    #[test]
    fn requeued_arbitrary_claim_joins_the_ready_tail() {
        let first = FiberId(1);
        let claimed = FiberId(2);
        let mut frontier = ReconciliationFrontier::default();

        frontier.enqueue(first);
        frontier.enqueue(claimed);
        assert!(frontier.claim(claimed));
        frontier.enqueue(claimed);
        frontier.finish(claimed, true);

        assert_eq!(frontier.take(1), Some(first));
        assert_eq!(frontier.take(2), Some(claimed));
        assert!(frontier.invariants_hold());
    }

    #[test]
    fn arbitrary_claim_tombstones_remain_amortized_and_bounded() {
        const FIBERS: u64 = 4_096;
        let mut frontier = ReconciliationFrontier::default();
        for raw in 1..=FIBERS {
            frontier.enqueue(FiberId(raw));
        }
        for raw in (1..=FIBERS).rev() {
            let id = FiberId(raw);
            assert!(frontier.claim(id));
            frontier.finish(id, false);
        }

        assert!(frontier.ready.is_empty());
        assert!(frontier.ready_order.len() <= ReconciliationFrontier::TOMBSTONE_HEADROOM);
        assert!(frontier.invariants_hold());
    }

    #[test]
    fn abandoned_and_removed_work_preserves_frontier_ownership() {
        let abandoned = FiberId(1);
        let removed = FiberId(2);
        let mut frontier = ReconciliationFrontier::default();

        frontier.enqueue(abandoned);
        assert!(frontier.claim(abandoned));
        frontier.abandon(abandoned, true);
        assert!(frontier.ready.contains_key(&abandoned));

        frontier.enqueue(removed);
        assert!(frontier.claim(removed));
        frontier.enqueue(removed);
        frontier.remove_queued(removed);
        frontier.finish(removed, true);
        assert!(!frontier.ready.contains_key(&removed));
        assert!(!frontier.rerun.contains(&removed));
        assert!(frontier.invariants_hold());
    }

    #[derive(Debug)]
    struct PassiveFactory;

    #[async_trait::async_trait]
    impl PluginFactory for PassiveFactory {
        fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
            Ok(PreparedActivation::new(desired.clone()))
        }

        async fn activate(&self, _: ActivationPlan) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct SaturationProvider;

    #[async_trait::async_trait]
    impl PluginFactory for SaturationProvider {
        fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
            Ok(PreparedActivation::new(desired.clone()))
        }

        async fn activate(&self, plan: ActivationPlan) -> Result<()> {
            plan.context().provide(
                "saturation-trigger",
                "test.saturation-trigger",
                ContractVersion(1),
                Arc::new(SaturationEndpoint),
            )?;
            Ok(())
        }
    }

    #[derive(Debug)]
    struct SaturationEndpoint;

    #[async_trait::async_trait]
    impl ServiceEndpoint for SaturationEndpoint {
        async fn serve(&self, _: InvocationContext, _: crate::ProviderChannel<'_>) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct GatedSaturationConsumer {
        started: Arc<tokio::sync::Semaphore>,
        release: Arc<tokio::sync::Semaphore>,
    }

    #[async_trait::async_trait]
    impl PluginFactory for GatedSaturationConsumer {
        fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
            Ok(
                PreparedActivation::new(desired.clone()).requiring(Requirement::new(
                    "saturation-trigger",
                    "test.saturation-trigger",
                    ContractVersion(1),
                )),
            )
        }

        async fn activate(&self, _: ActivationPlan) -> Result<()> {
            self.started.add_permits(1);
            self.release
                .acquire()
                .await
                .expect("the activation gate remains open")
                .forget();
            Ok(())
        }
    }

    #[derive(Debug)]
    struct PanickingDropFactory;

    impl Drop for PanickingDropFactory {
        fn drop(&mut self) {
            panic!("factory drop panic evidence");
        }
    }

    #[async_trait::async_trait]
    impl PluginFactory for PanickingDropFactory {
        fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
            Ok(PreparedActivation::new(desired.clone()))
        }

        async fn activate(&self, _: ActivationPlan) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn concurrent_settlement_cannot_overtake_ticket_registration() {
        let snapshot = FiberSnapshot {
            id: FiberId(1),
            generation: FiberGeneration(1),
            factory: FactoryIdentity::linked("settlement-race", "1"),
            update_mode: UpdateMode::Replayable,
            state: FiberState::Active,
        };
        for _ in 0..1_024 {
            let (watch, _) = watch::channel(0_u64);
            let progress = Arc::new(ReconciliationProgress {
                desired: AtomicU64::new(0),
                settled: AtomicU64::new(0),
                watch,
                completions: Mutex::new(BTreeMap::new()),
            });
            let requester = std::thread::spawn({
                let progress = Arc::clone(&progress);
                move || progress.request()
            });
            while progress.desired() == 0 {
                std::hint::spin_loop();
            }
            // `begin_run` shares the completion lock with registration, so
            // observing the desired revision cannot capture a partial ticket.
            let run = progress.begin_run();
            assert_eq!(run.revision, 1);
            progress.mark_settled(&run, &snapshot);
            let ticket = requester.join().expect("requester remains healthy");
            assert_eq!(
                *ticket
                    .completion
                    .snapshot
                    .lock()
                    .expect("reconciliation completion poisoned"),
                Some(snapshot.clone())
            );
        }
    }

    #[tokio::test]
    async fn terminal_finish_settles_tickets_beyond_a_saturated_run_prefix() {
        let (watch, _) = watch::channel(u64::MAX);
        let progress = ReconciliationProgress {
            desired: AtomicU64::new(u64::MAX),
            settled: AtomicU64::new(u64::MAX),
            watch,
            completions: Mutex::new(BTreeMap::new()),
        };
        let current = progress.request();
        let run = progress.begin_run();
        let later = progress.request();
        let current_snapshot = FiberSnapshot {
            id: FiberId(1),
            generation: FiberGeneration(1),
            factory: FactoryIdentity::linked("saturated-finish", "1"),
            update_mode: UpdateMode::Replayable,
            state: FiberState::Failed("current run".to_owned()),
        };
        progress.mark_settled(&run, &current_snapshot);
        assert_eq!(current.join().await, current_snapshot);

        let terminal_snapshot = FiberSnapshot {
            id: FiberId(1),
            generation: FiberGeneration(1),
            factory: FactoryIdentity::linked("saturated-finish", "1"),
            update_mode: UpdateMode::Replayable,
            state: FiberState::Disposed,
        };
        progress.finish(&terminal_snapshot);
        assert_eq!(later.join().await, terminal_snapshot);
    }

    #[tokio::test]
    async fn saturated_revision_still_settles_new_tickets() {
        let runtime = Runtime::default();
        let fiber = runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "saturated-revision",
                    "1",
                    UpdateMode::Replayable,
                    Arc::new(PassiveFactory),
                ),
                ConfigValue::Null,
            )
            .await
            .unwrap();
        runtime.wait_scheduler_idle().await;
        fiber
            .fiber
            .reconciliation
            .desired
            .store(u64::MAX, Ordering::Release);
        fiber
            .fiber
            .reconciliation
            .settled
            .store(u64::MAX, Ordering::Release);
        fiber.fiber.reconciliation.watch.send_replace(u64::MAX);

        let ticket = runtime
            .request_reconciliation(fiber.id())
            .expect("a registered Fiber accepts reconciliation");
        tokio::time::timeout(std::time::Duration::from_millis(100), ticket.join())
            .await
            .expect("a ticket created after revision saturation remained pending");

        assert!(fiber.dispose().await.is_clean());
        assert!(runtime.shutdown().await.is_complete());
    }

    #[tokio::test(start_paused = true)]
    async fn saturated_reconfigure_waits_for_the_run_that_includes_it() {
        let runtime = Runtime::default();
        let started = Arc::new(tokio::sync::Semaphore::new(0));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let consumer = runtime
            .root()
            .apply(
                crate::plugin::resolved_test_factory(Arc::new(GatedSaturationConsumer {
                    started: Arc::clone(&started),
                    release: Arc::clone(&release),
                })),
                ConfigValue::Null,
            )
            .await
            .unwrap();
        assert!(matches!(consumer.snapshot().state, FiberState::Pending(_)));
        runtime.wait_scheduler_idle().await;
        consumer
            .fiber
            .reconciliation
            .desired
            .store(u64::MAX, Ordering::Release);
        consumer
            .fiber
            .reconciliation
            .settled
            .store(u64::MAX, Ordering::Release);
        consumer.fiber.reconciliation.watch.send_replace(u64::MAX);

        let provider = runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "saturation-provider",
                    "1",
                    UpdateMode::Replayable,
                    Arc::new(SaturationProvider),
                ),
                ConfigValue::Null,
            )
            .await
            .unwrap();
        started
            .acquire()
            .await
            .expect("the activation-start gate remains open")
            .forget();

        let reconfigure_consumer = consumer.clone();
        let mut reconfigure = tokio::spawn(async move {
            reconfigure_consumer
                .reconfigure(serde_json::json!({"revision": 2}))
                .await
        });
        started
            .acquire()
            .await
            .expect("the activation-start gate remains open")
            .forget();

        if let Ok(result) =
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut reconfigure).await
        {
            release.add_permits(1);
            panic!("reconfiguration completed before its reconciliation run: {result:?}");
        }

        release.add_permits(1);
        let snapshot = reconfigure.await.unwrap().unwrap();
        assert!(matches!(snapshot.state, FiberState::Active));
        assert!(consumer.dispose().await.is_clean());
        assert!(provider.dispose().await.is_clean());
        assert!(runtime.shutdown().await.is_complete());
    }

    #[tokio::test(start_paused = true)]
    async fn late_disposal_panic_settles_registered_reconciliation_ticket() {
        let runtime = Runtime::default();
        let fiber = runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "late-disposal-panic",
                    "1",
                    UpdateMode::Replayable,
                    Arc::new(PanickingDropFactory),
                ),
                ConfigValue::Null,
            )
            .await
            .unwrap();
        runtime.wait_scheduler_idle().await;

        // Register work and request disposal without yielding, so the one
        // scheduler worker observes both intents in its first poll.
        let ticket = runtime
            .request_reconciliation(fiber.id())
            .expect("a registered Fiber accepts reconciliation");
        runtime.request_disposal(&fiber.fiber);

        let report = fiber.dispose().await;
        assert!(report.is_clean());
        let snapshot = tokio::time::timeout(std::time::Duration::from_millis(100), ticket.join())
            .await
            .expect("terminal disposal left a registered reconciliation ticket pending");
        assert!(matches!(snapshot.state, FiberState::Disposed));
    }
}
