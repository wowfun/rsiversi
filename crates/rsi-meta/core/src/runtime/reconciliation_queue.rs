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

struct CycleEdges {
    descriptor: Arc<PreparedDescriptor>,
    edges: Vec<(FiberId, usize)>,
}

struct ReconciliationRun {
    revision: u64,
    saturated_completion_prefix: Option<usize>,
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
        let removed = state.active_reconciliations.remove(&self.id);
        debug_assert!(removed);
        if state.fibers.contains_key(&self.id) {
            state.pending_reconciliations.insert(self.id);
        }
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
    pub(super) fn dependency_cycle(
        &self,
        start: FiberId,
        maximum_services: usize,
        maximum_bytes: usize,
    ) -> Option<(Vec<ServiceKey>, bool)> {
        struct CycleFrame {
            descriptor: Arc<PreparedDescriptor>,
            edges: Vec<(FiberId, usize)>,
            next_edge: usize,
            restore_services: usize,
            restore_bytes: usize,
            restore_truncated: bool,
        }

        let CycleEdges { descriptor, edges } = self.cycle_edges(start)?;
        let mut visited = BTreeSet::from([start]);
        let mut stack = vec![CycleFrame {
            descriptor,
            edges,
            next_edge: 0,
            restore_services: 0,
            restore_bytes: 0,
            restore_truncated: false,
        }];
        let mut services = Vec::new();
        let mut retained_bytes = 0_usize;
        let mut truncated = false;
        while let Some(frame) = stack.last_mut() {
            let Some((next, requirement_index)) = frame.edges.get(frame.next_edge).copied() else {
                let frame = stack.pop().expect("cycle stack has a current frame");
                services.truncate(frame.restore_services);
                retained_bytes = frame.restore_bytes;
                truncated = frame.restore_truncated;
                continue;
            };
            frame.next_edge += 1;
            let restore_services = services.len();
            let restore_bytes = retained_bytes;
            let restore_truncated = truncated;
            if !truncated {
                let service = &frame.descriptor.requires[requirement_index].key;
                let next_bytes = retained_bytes.checked_add(service.as_str().len());
                if services.len() < maximum_services
                    && next_bytes.is_some_and(|bytes| bytes <= maximum_bytes)
                {
                    retained_bytes = next_bytes.expect("checked bounded service bytes");
                    services.push(service.clone());
                } else {
                    truncated = true;
                }
            }
            if next == start {
                return Some((services, truncated));
            }
            if visited.insert(next)
                && let Some(CycleEdges { descriptor, edges }) = self.cycle_edges(next)
            {
                stack.push(CycleFrame {
                    descriptor,
                    edges,
                    next_edge: 0,
                    restore_services,
                    restore_bytes,
                    restore_truncated,
                });
                continue;
            }
            services.truncate(restore_services);
            retained_bytes = restore_bytes;
            truncated = restore_truncated;
        }
        None
    }

    fn cycle_edges(&self, id: FiberId) -> Option<CycleEdges> {
        let fiber = {
            let state = self.inner.state.lock().expect("runtime state poisoned");
            state.fibers.get(&id).cloned()
        }?;
        let descriptor = {
            let data = fiber.data.lock().expect("fiber state poisoned");
            if data.disposed {
                return None;
            }
            let descriptor = Arc::clone(
                data.descriptor
                    .as_ref()
                    .expect("registered Fiber retains its descriptor"),
            );
            if let Some(active) = &data.active {
                let edges = descriptor
                    .requires
                    .iter()
                    .enumerate()
                    .filter_map(|(index, requirement)| {
                        active
                            .bindings
                            .get(&requirement.key)
                            .map(|binding| (binding.provider, index))
                    })
                    .collect();
                return Some(CycleEdges { descriptor, edges });
            }
            // Pending and Failed fibers have no actual bindings. Their
            // validated declarations remain graph edges because Failed is a
            // recoverable state: reconfiguration can reactivate the same Fiber.
            descriptor
        };
        let state = self.inner.state.lock().expect("runtime state poisoned");
        let edges = descriptor
            .requires
            .iter()
            .enumerate()
            .flat_map(|(index, requirement)| {
                state
                    .declarations
                    .providers(&fiber.base_context, requirement)
                    .into_iter()
                    .map(move |provider| (provider, index))
            })
            .collect();
        Some(CycleEdges { descriptor, edges })
    }

    pub(super) fn request_reconciliation(&self, id: FiberId) -> Option<ReconciliationTicket> {
        let (ticket, should_spawn) = {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            let fiber = state.fibers.get(&id).cloned()?;
            let ticket = fiber.reconciliation.request();
            fiber.cancel_loading_activation();
            state.pending_reconciliations.insert(id);
            let should_spawn = if state.reconciliation_worker_running {
                false
            } else {
                state.reconciliation_worker_running = true;
                true
            };
            (ticket, should_spawn)
        };
        if should_spawn {
            self.spawn_reconciliation_worker();
        }
        self.inner.scheduler_wakeup.notify_one();
        Some(ticket)
    }

    pub(super) fn request_disposal(&self, fiber: &Arc<Fiber>) {
        if !fiber.disposal.try_start() {
            return;
        }
        fiber.disposal_requested.cancel();
        fiber.cancel_loading_activation();
        self.enqueue_intent(fiber.id);
    }

    fn enqueue_intent(&self, id: FiberId) {
        let should_spawn = {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            if !state.fibers.contains_key(&id) {
                return;
            }
            state.pending_reconciliations.insert(id);
            if state.reconciliation_worker_running {
                false
            } else {
                state.reconciliation_worker_running = true;
                true
            }
        };
        if should_spawn {
            self.spawn_reconciliation_worker();
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
            if state.active_reconciliations.contains(&id) {
                false
            } else if state.pending_reconciliations.remove(&id) {
                let inserted = state.active_reconciliations.insert(id);
                debug_assert!(inserted);
                true
            } else {
                false
            }
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

    pub(super) fn refresh_pending_diagnostics(
        &self,
        services: &[ServiceSlot],
        except: Option<FiberId>,
    ) {
        let should_spawn = {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            let affected = Self::dependent_ids(&state, services, except)
                .into_iter()
                .filter(|id| {
                    state.fibers.get(id).is_some_and(|fiber| {
                        matches!(
                            fiber.data.lock().expect("fiber state poisoned").state,
                            FiberState::Pending(_)
                        )
                    })
                })
                .collect::<Vec<_>>();
            for id in affected {
                if let Some(fiber) = state.fibers.get(&id) {
                    fiber.reconciliation.request_revision();
                    state.pending_reconciliations.insert(id);
                }
            }
            if state.pending_reconciliations.is_empty() || state.reconciliation_worker_running {
                false
            } else {
                state.reconciliation_worker_running = true;
                true
            }
        };
        if should_spawn {
            self.spawn_reconciliation_worker();
        }
        self.inner.scheduler_wakeup.notify_one();
    }

    pub(super) fn notify_service_changes(&self, services: &[ServiceSlot], except: Option<FiberId>) {
        let should_spawn = {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            let affected = Self::dependent_ids(&state, services, except);
            for id in affected {
                if let Some(fiber) = state.fibers.get(&id) {
                    fiber.reconciliation.request_revision();
                    fiber.cancel_loading_activation();
                    state.pending_reconciliations.insert(id);
                }
            }
            if state.pending_reconciliations.is_empty() || state.reconciliation_worker_running {
                false
            } else {
                state.reconciliation_worker_running = true;
                true
            }
        };
        if should_spawn {
            self.spawn_reconciliation_worker();
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

    fn spawn_reconciliation_worker(&self) {
        let usage = self
            .inner
            .resources
            .scheduler_workers
            .try_reserve(1)
            .expect("one scheduler-running flag owns the single worker reservation");
        let runtime = self.clone();
        tokio::spawn(async move { runtime.run_reconciliation_worker(usage).await });
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
                    runtime.drive_transition(id).await;
                    id
                }));
            }
            if active.is_empty() {
                let should_stop = {
                    let mut state = self.inner.state.lock().expect("runtime state poisoned");
                    if state.pending_reconciliations.is_empty() {
                        state.reconciliation_worker_running = false;
                        // State serialization keeps the running flag and the
                        // one-worker ledger at one linearization point.
                        worker_usage.shrink_to(0);
                        true
                    } else {
                        false
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
        if state.active_reconciliations.len() >= active_limit {
            return None;
        }
        let id = state
            .pending_reconciliations
            .iter()
            .find(|id| !state.active_reconciliations.contains(id))
            .copied()?;
        state.pending_reconciliations.remove(&id);
        let inserted = state.active_reconciliations.insert(id);
        debug_assert!(inserted);
        Some(id)
    }

    fn finish_scheduled_intent(&self, id: FiberId) {
        let removed = self
            .inner
            .state
            .lock()
            .expect("runtime state poisoned")
            .active_reconciliations
            .remove(&id);
        debug_assert!(removed);
        self.inner.scheduler_wakeup.notify_one();
    }

    pub(super) async fn wait_scheduler_idle(&self) {
        loop {
            let notified = self.inner.scheduler_idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let idle = {
                let state = self.inner.state.lock().expect("runtime state poisoned");
                !state.reconciliation_worker_running
                    && state.pending_reconciliations.is_empty()
                    && state.active_reconciliations.is_empty()
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
        if fiber.reconciliation.desired() > revision {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            if state.fibers.contains_key(&id) {
                state.pending_reconciliations.insert(id);
            }
            drop(state);
            self.inner.scheduler_wakeup.notify_one();
        }
    }

    pub(super) async fn reconcile_service_changes(
        &self,
        services: &[ServiceSlot],
        except: Option<FiberId>,
    ) {
        let affected = {
            let state = self.inner.state.lock().expect("runtime state poisoned");
            Self::dependent_ids(&state, services, except)
        };
        let tickets = affected
            .into_iter()
            .filter_map(|id| self.request_reconciliation(id).map(|ticket| (id, ticket)))
            .collect::<Vec<_>>();
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

    #[derive(Debug)]
    struct PassiveFactory(PluginDescriptor);

    #[async_trait::async_trait]
    impl PluginFactory for PassiveFactory {
        fn descriptor(&self) -> &PluginDescriptor {
            &self.0
        }

        async fn activate(&self, _: Context, _: Arc<ConfigValue>) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct SaturationProvider(PluginDescriptor);

    #[async_trait::async_trait]
    impl PluginFactory for SaturationProvider {
        fn descriptor(&self) -> &PluginDescriptor {
            &self.0
        }

        async fn activate(&self, context: Context, _: Arc<ConfigValue>) -> Result<()> {
            context.provide(
                "saturation-trigger",
                "test.saturation-trigger",
                ContractVersion(1),
                Arc::new(SaturationEndpoint),
            )
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
        descriptor: PluginDescriptor,
        started: Arc<tokio::sync::Semaphore>,
        release: Arc<tokio::sync::Semaphore>,
    }

    #[async_trait::async_trait]
    impl PluginFactory for GatedSaturationConsumer {
        fn descriptor(&self) -> &PluginDescriptor {
            &self.descriptor
        }

        async fn activate(&self, _: Context, _: Arc<ConfigValue>) -> Result<()> {
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
    struct PanickingDropFactory(PluginDescriptor);

    impl Drop for PanickingDropFactory {
        fn drop(&mut self) {
            panic!("factory drop panic evidence");
        }
    }

    #[async_trait::async_trait]
    impl PluginFactory for PanickingDropFactory {
        fn descriptor(&self) -> &PluginDescriptor {
            &self.0
        }

        async fn activate(&self, _: Context, _: Arc<ConfigValue>) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn concurrent_settlement_cannot_overtake_ticket_registration() {
        let snapshot = FiberSnapshot {
            id: FiberId(1),
            generation: FiberGeneration(1),
            factory: FactoryIdentity::builtin("settlement-race", "1"),
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
            factory: FactoryIdentity::builtin("saturated-finish", "1"),
            state: FiberState::Failed("current run".to_owned()),
        };
        progress.mark_settled(&run, &current_snapshot);
        assert_eq!(current.join().await, current_snapshot);

        let terminal_snapshot = FiberSnapshot {
            id: FiberId(1),
            generation: FiberGeneration(1),
            factory: FactoryIdentity::builtin("saturated-finish", "1"),
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
                Arc::new(PassiveFactory(PluginDescriptor::new(
                    FactoryIdentity::builtin("saturated-revision", "1"),
                ))),
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
                Arc::new(GatedSaturationConsumer {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                        "saturated-consumer",
                        "1",
                    ))
                    .requiring(crate::Requirement::new(
                        "saturation-trigger",
                        "test.saturation-trigger",
                        ContractVersion(1),
                    )),
                    started: Arc::clone(&started),
                    release: Arc::clone(&release),
                }),
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
                Arc::new(SaturationProvider(
                    PluginDescriptor::new(FactoryIdentity::builtin("saturation-provider", "1"))
                        .providing(crate::Provision::new(
                            "saturation-trigger",
                            "test.saturation-trigger",
                            ContractVersion(1),
                        )),
                )),
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
                Arc::new(PanickingDropFactory(PluginDescriptor::new(
                    FactoryIdentity::builtin("late-disposal-panic", "1"),
                ))),
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
        assert_eq!(report.failures.len(), 1);
        let snapshot = tokio::time::timeout(std::time::Duration::from_millis(100), ticket.join())
            .await
            .expect("terminal disposal left a registered reconciliation ticket pending");
        assert!(matches!(snapshot.state, FiberState::Disposed));
    }
}
