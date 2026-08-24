#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;
impl CleanupRun {
    fn try_start(&self) -> bool {
        self.started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn finish(&self, report: CleanupReport) {
        *self.result.lock().expect("cleanup run poisoned") = Some(report);
        self.complete.notify_waiters();
    }

    async fn join(&self) -> CleanupReport {
        loop {
            let notified = self.complete.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(report) = self.result.lock().expect("cleanup run poisoned").clone() {
                return report;
            }
            notified.as_mut().await;
        }
    }
}

impl DisposalRun {
    pub(super) fn try_start(&self) -> bool {
        self.started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn finish(&self, result: DisposalResult) {
        *self.result.lock().expect("disposal run poisoned") = Some(result);
        self.complete.notify_waiters();
    }

    async fn join(&self) -> DisposalResult {
        loop {
            let notified = self.complete.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(result) = self.result.lock().expect("disposal run poisoned").clone() {
                return result;
            }
            notified.as_mut().await;
        }
    }
}

impl ShutdownRun {
    fn add_report(&self, report: CleanupReport, maximum_entries: usize, maximum_bytes: usize) {
        let mut state = self.state.lock().expect("shutdown run poisoned");
        state
            .report
            .extend_bounded(report, maximum_entries, maximum_bytes);
        self.complete.notify_waiters();
    }

    fn finish(&self) {
        let mut state = self.state.lock().expect("shutdown run poisoned");
        state.outcome = Some(state.report.clone());
        self.complete.notify_waiters();
    }

    fn fail(&self, report: CleanupReport, maximum_entries: usize, maximum_bytes: usize) {
        let mut state = self.state.lock().expect("shutdown run poisoned");
        state
            .report
            .extend_bounded(report, maximum_entries, maximum_bytes);
        state.failed = true;
        self.complete.notify_waiters();
    }

    fn mark_failed(&self) {
        self.state.lock().expect("shutdown run poisoned").failed = true;
        self.complete.notify_waiters();
    }

    pub(super) fn is_complete(&self) -> bool {
        self.state
            .lock()
            .expect("shutdown run poisoned")
            .outcome
            .is_some()
    }

    fn snapshot(&self) -> (CleanupReport, Option<CleanupReport>, bool) {
        let state = self.state.lock().expect("shutdown run poisoned");
        (state.report.clone(), state.outcome.clone(), state.failed)
    }
}

impl Runtime {
    pub(super) async fn rollback_loading(&self, fiber: &Arc<Fiber>) -> CleanupReport {
        self.cleanup_generation(fiber).await
    }

    pub(super) async fn unload_generation(&self, fiber: &Arc<Fiber>) -> CleanupReport {
        self.cleanup_generation(fiber).await
    }

    fn cleanup_generation(&self, fiber: &Arc<Fiber>) -> BoxFuture<'static, CleanupReport> {
        let runtime = self.clone();
        let fiber = Arc::clone(fiber);
        Box::pin(async move {
            let (run, claimed) = {
                let mut data = fiber.data.lock().expect("fiber state poisoned");
                let Some(active) = data.active.as_mut() else {
                    return CleanupReport::default();
                };
                let run = Arc::clone(&active.cleanup);
                let claimed = run.try_start().then(|| ClaimedCleanup {
                    generation: active.generation,
                    services: active
                        .services
                        .values()
                        .map(|service| Arc::clone(&service.binding))
                        .collect::<Vec<_>>(),
                    listener_ids: active.listeners.keys().copied().collect::<BTreeSet<_>>(),
                    published: active.published,
                    lease: Arc::clone(&active.lease),
                    children: std::mem::take(&mut active.children),
                    retired_child_report: std::mem::take(&mut active.retired_child_report),
                    effects: std::mem::take(&mut active.effects),
                });
                if claimed.is_some() {
                    data.state = FiberState::Unloading;
                    let snapshot = data.snapshot(fiber.id);
                    fiber.watch.send_replace(snapshot);
                }
                (run, claimed)
            };
            if let Some(claimed) = claimed {
                let generation = claimed.generation;
                let cleanup_usage = runtime
                    .inner
                    .resources
                    .cleanup_runs
                    .try_reserve(1)
                    .expect("one cleanup run per registered Fiber fits the Fiber limit");
                let cleanup_runtime = runtime.clone();
                let cleanup_fiber = Arc::clone(&fiber);
                let owned_run = Arc::clone(&run);
                tokio::spawn(async move {
                    let cleanup = std::panic::AssertUnwindSafe(
                        cleanup_runtime.with_reconciliation_slot(
                            cleanup_runtime
                                .run_claimed_cleanup(Arc::clone(&cleanup_fiber), claimed),
                        ),
                    )
                    .catch_unwind()
                    .await;
                    let report = cleanup.unwrap_or_else(|_| {
                        let mut data = cleanup_fiber.data.lock().expect("fiber state poisoned");
                        if data
                            .active
                            .as_ref()
                            .is_some_and(|active| active.generation == generation)
                        {
                            data.active = None;
                        }
                        drop(data);
                        // A panic outside the per-effect boundary means the
                        // cleanup transaction could not prove that every
                        // publication was withdrawn. Fail closed instead of
                        // allowing a potentially stale provider generation to
                        // remain authoritative.
                        cleanup_runtime.mark_terminal_owned("runtime cleanup driver panicked");
                        let mut report = CleanupReport::default();
                        report.push_bounded(
                            format!("fiber {} cleanup", cleanup_fiber.id.0),
                            "cleanup run panicked",
                            cleanup_runtime
                                .inner
                                .limits
                                .payloads
                                .maximum_diagnostic_entries,
                            cleanup_runtime
                                .inner
                                .limits
                                .payloads
                                .maximum_diagnostic_bytes,
                        );
                        report
                    });
                    // A completed cleanup result must imply zero unfinished-run
                    // usage to every waiter woken by `finish`.
                    drop(cleanup_usage);
                    owned_run.finish(report);
                });
            }
            runtime.yield_reconciliation_slot(run.join()).await
        })
    }

    async fn run_claimed_cleanup(
        &self,
        fiber: Arc<Fiber>,
        claimed: ClaimedCleanup,
    ) -> CleanupReport {
        let ClaimedCleanup {
            generation,
            services,
            listener_ids,
            published,
            lease,
            children,
            retired_child_report,
            mut effects,
        } = claimed;
        let maximum_entries = self.inner.limits.payloads.maximum_diagnostic_entries;
        let maximum_bytes = self.inner.limits.payloads.maximum_diagnostic_bytes;
        let mut report = retired_child_report;
        *fiber.cleanup_phase.lock().expect("cleanup phase poisoned") = CleanupPhase::Withdrawing;
        lease.close();
        let changed = self.withdraw_generation_services(&fiber, generation, &services, published);
        let owner = Owner {
            fiber: fiber.id,
            generation,
        };
        for id in listener_ids {
            self.remove_listener_owned(owner, id, ListenerRemovalCause::Retirement);
        }
        *fiber.cleanup_phase.lock().expect("cleanup phase poisoned") =
            CleanupPhase::WaitingForDependents;
        self.reconcile_service_changes(&changed, Some(fiber.id))
            .await;
        *fiber.cleanup_phase.lock().expect("cleanup phase poisoned") =
            CleanupPhase::DrainingAdmissions;
        // Dependent convergence is the last point at which an already
        // retiring generation may legitimately start a call. Seal before the
        // drain so a caller classified from stale state cannot reopen this
        // provider after cleanup has observed zero live callbacks.
        lease.seal();
        self.yield_reconciliation_slot(async {
            lease.wait_drained().await;
            *fiber.cleanup_phase.lock().expect("cleanup phase poisoned") =
                CleanupPhase::DisposingChildren;
            for child in children.into_iter().rev() {
                report.extend_bounded(
                    self.dispose_fiber_instance_result(child).await.report,
                    maximum_entries,
                    maximum_bytes,
                );
            }
            *fiber.cleanup_phase.lock().expect("cleanup phase poisoned") =
                CleanupPhase::RunningEffects;
            while let Some(effect) = effects.pop() {
                match std::panic::AssertUnwindSafe(async { (effect.cleanup)().await })
                    .catch_unwind()
                    .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        report.push_bounded(effect.label, error, maximum_entries, maximum_bytes);
                    }
                    Err(_) => report.push_bounded(
                        effect.label,
                        "cleanup panicked",
                        maximum_entries,
                        maximum_bytes,
                    ),
                }
            }
            let mut data = fiber.data.lock().expect("fiber state poisoned");
            if data
                .active
                .as_ref()
                .is_some_and(|active| active.generation == generation)
            {
                data.active = None;
            }
        })
        .await;
        report
    }

    fn withdraw_generation_services(
        &self,
        fiber: &Fiber,
        generation: FiberGeneration,
        services: &[Arc<ProviderBinding>],
        published: bool,
    ) -> Vec<ServiceSlot> {
        let mut changed = Vec::new();
        {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            if published {
                for binding in services {
                    let slot = fiber.base_context.service_slot(&binding.key);
                    if state.providers.get(&slot).is_some_and(|current| {
                        current.provider == fiber.id && current.generation == generation
                    }) {
                        state.providers.remove(&slot);
                        changed.push(slot);
                    }
                }
            }
            if !changed.is_empty() {
                state.revision += 1;
            }
        }
        changed
    }

    pub(super) fn dispose_fiber_instance(
        &self,
        fiber: Arc<Fiber>,
    ) -> BoxFuture<'static, CleanupReport> {
        let runtime = self.clone();
        Box::pin(async move { runtime.dispose_fiber_instance_result(fiber).await.report })
    }

    fn dispose_fiber_instance_result(
        &self,
        fiber: Arc<Fiber>,
    ) -> BoxFuture<'static, DisposalResult> {
        let runtime = self.clone();
        Box::pin(async move {
            let run = Arc::clone(&fiber.disposal);
            runtime.request_disposal(&fiber);
            runtime.drive_nested_intent(fiber.id).await;
            run.join().await
        })
    }

    pub(super) async fn run_scheduled_disposal(&self, fiber: Arc<Fiber>) {
        if fiber
            .disposal
            .result
            .lock()
            .expect("disposal run poisoned")
            .is_some()
        {
            return;
        }
        let result = std::panic::AssertUnwindSafe(
            self.with_reconciliation_slot(self.dispose_fiber_owned(Arc::clone(&fiber))),
        )
        .catch_unwind()
        .await;
        let result = result.map_or_else(
            |_| {
                self.mark_terminal_owned(format!(
                    "runtime-owned Fiber {} disposal panicked",
                    fiber.id.0
                ));
                let mut report = CleanupReport::default();
                report.push_bounded(
                    format!("fiber {} disposal", fiber.id.0),
                    "disposal run panicked",
                    self.inner.limits.payloads.maximum_diagnostic_entries,
                    self.inner.limits.payloads.maximum_diagnostic_bytes,
                );
                DisposalResult {
                    report,
                    quiescent: false,
                }
            },
            |report| DisposalResult {
                report,
                quiescent: true,
            },
        );
        fiber.disposal.finish(result);
    }

    async fn dispose_fiber_owned(&self, fiber: Arc<Fiber>) -> CleanupReport {
        let id = fiber.id;
        {
            let mut data = fiber.data.lock().expect("fiber state poisoned");
            data.disposed = true;
        }
        let report = self.unload_generation(&fiber).await;
        fiber.set_state(FiberState::Disposed);
        let (required_slots, declared_slots) = {
            let data = fiber.data.lock().expect("fiber state poisoned");
            let descriptor = data
                .descriptor
                .as_deref()
                .expect("registered Fiber retains its descriptor");
            (
                descriptor
                    .required_services()
                    .map(|service| fiber.base_context.service_slot(service))
                    .collect::<Vec<_>>(),
                descriptor
                    .provided_services()
                    .map(|service| fiber.base_context.service_slot(service))
                    .collect::<Vec<_>>(),
            )
        };
        {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            state.fibers.remove(&id);
            state.declarations.remove(id);
            state.pending_reconciliations.remove(&id);
            for slot in required_slots {
                let remove_entry = state.dependents.get_mut(&slot).is_some_and(|fibers| {
                    fibers.remove(&id);
                    fibers.is_empty()
                });
                if remove_entry {
                    state.dependents.remove(&slot);
                }
            }
            if let Some(parent) = fiber.parent
                && let Some(parent_fiber) = state.fibers.get(&parent.fiber)
            {
                let mut data = parent_fiber.data.lock().expect("fiber state poisoned");
                if data.generation == parent.generation
                    && let Some(active) = data.active.as_mut()
                {
                    let previous = active.children.len();
                    active.children.retain(|child| child.id != id);
                    if active.children.len() != previous {
                        active.retired_child_report.extend_bounded(
                            report.clone(),
                            self.inner.limits.payloads.maximum_diagnostic_entries,
                            self.inner.limits.payloads.maximum_diagnostic_bytes,
                        );
                    }
                }
            }
            state.revision += 1;
        }
        self.refresh_pending_diagnostics(&declared_slots, Some(id));
        // Capacity belongs to registry membership, not to management handles
        // that may outlive disposal.
        {
            let mut data = fiber.data.lock().expect("fiber state poisoned");
            data.reservations.take();
            data.factory = None;
            data.descriptor = None;
            data.config = None;
            data.last_attempt = None;
        }
        fiber.reconciliation.finish(&fiber.snapshot());
        report
    }

    /// Closes admission and waits once for persistent Runtime-owned teardown.
    ///
    /// The first caller starts one shutdown run. A waiter timeout does not
    /// cancel cleanup; a later caller joins the same run and can observe
    /// [`ShutdownOutcome::Complete`] after actual quiescence.
    pub async fn shutdown(&self) -> ShutdownOutcome {
        if !self.inner.shutting_down.swap(true, Ordering::AcqRel) {
            self.inner.runtime_admission.close();
            // The Runtime is terminal for new calls and dispatches. Cancelling
            // their shared drivers ensures shutdown can wait for actual
            // quiescence instead of allowing callbacks to start after teardown.
            self.inner.terminal_cancellation.cancel();
            let roots = self.shutdown_membership_snapshot();
            let runtime = self.clone();
            tokio::spawn(async move {
                let result = std::panic::AssertUnwindSafe(runtime.shutdown_inner(roots))
                    .catch_unwind()
                    .await;
                if result.is_err() {
                    let mut report = CleanupReport::default();
                    report.push_bounded(
                        "runtime shutdown".to_owned(),
                        "shutdown driver panicked",
                        runtime.inner.limits.payloads.maximum_diagnostic_entries,
                        runtime.inner.limits.payloads.maximum_diagnostic_bytes,
                    );
                    // Publish the persistent failure before terminalization so
                    // even a poisoned Runtime-state mutex cannot strand every
                    // later shutdown waiter behind its full deadline.
                    runtime.inner.shutdown.fail(
                        report,
                        runtime.inner.limits.payloads.maximum_diagnostic_entries,
                        runtime.inner.limits.payloads.maximum_diagnostic_bytes,
                    );
                    runtime.mark_terminal_owned("runtime shutdown driver panicked");
                }
            });
        }

        let deadline = tokio::time::Instant::now()
            .checked_add(self.inner.limits.deadlines.shutdown_wait)
            .expect("validated shutdown deadline fits Tokio Instant");
        loop {
            let notified = self.inner.shutdown.complete.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let (report, complete, failed) = self.inner.shutdown.snapshot();
            if let Some(report) = complete {
                return ShutdownOutcome::Complete(report);
            }
            if failed {
                return ShutdownOutcome::Failed {
                    report,
                    unresolved: self.unresolved_cleanup_report(),
                };
            }
            if tokio::time::Instant::now() >= deadline {
                return ShutdownOutcome::TimedOut {
                    report,
                    unresolved: self.unresolved_cleanup_report(),
                };
            }
            if tokio::time::timeout_at(deadline, notified.as_mut())
                .await
                .is_err()
            {
                let (report, complete, failed) = self.inner.shutdown.snapshot();
                return if let Some(report) = complete {
                    ShutdownOutcome::Complete(report)
                } else if failed {
                    ShutdownOutcome::Failed {
                        report,
                        unresolved: self.unresolved_cleanup_report(),
                    }
                } else {
                    ShutdownOutcome::TimedOut {
                        report,
                        unresolved: self.unresolved_cleanup_report(),
                    }
                };
            }
        }
    }

    fn shutdown_membership_snapshot(&self) -> Vec<Arc<Fiber>> {
        let state = self.inner.state.lock().expect("runtime state poisoned");
        state
            .fibers
            .values()
            .filter(|fiber| fiber.parent.is_none())
            .cloned()
            .collect()
    }

    fn unresolved_cleanup_report(&self) -> UnresolvedCleanupReport {
        let entry_limit = self.inner.limits.payloads.maximum_diagnostic_entries;
        let byte_limit = self.inner.limits.payloads.maximum_diagnostic_bytes / 32;
        let sample_limit = entry_limit.min(byte_limit).min(256);
        let (total, fibers) = {
            let state = self.inner.state.lock().expect("runtime state poisoned");
            (
                state.fibers.len(),
                state
                    .fibers
                    .values()
                    .take(sample_limit)
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        };
        let samples = fibers
            .into_iter()
            .map(|fiber| {
                let generation = fiber.data.lock().expect("fiber state poisoned").generation;
                let phase = *fiber.cleanup_phase.lock().expect("cleanup phase poisoned");
                UnresolvedCleanup {
                    fiber: fiber.id,
                    generation,
                    phase,
                }
            })
            .collect::<Vec<_>>();
        UnresolvedCleanupReport {
            total,
            truncated: samples.len() < total,
            samples,
        }
    }

    async fn shutdown_inner(&self, roots: Vec<Arc<Fiber>>) {
        let maximum_entries = self.inner.limits.payloads.maximum_diagnostic_entries;
        let maximum_bytes = self.inner.limits.payloads.maximum_diagnostic_bytes;
        let concurrency = self
            .inner
            .limits
            .execution
            .maximum_concurrent_reconciliations;
        let disposal_runs = roots
            .into_iter()
            .rev()
            .map(|root| {
                self.request_disposal(&root);
                Arc::clone(&root.disposal)
            })
            .collect::<Vec<_>>();
        let mut disposals = futures_util::stream::iter(disposal_runs)
            .map(|run| async move { run.join().await })
            .buffer_unordered(concurrency);
        let mut quiescent = true;
        while let Some(result) = disposals.next().await {
            quiescent &= result.quiescent;
            self.inner
                .shutdown
                .add_report(result.report, maximum_entries, maximum_bytes);
        }
        let unresolved = self.unresolved_cleanup_report();
        if quiescent && unresolved.total == 0 {
            self.wait_scheduler_idle().await;
            // No registered cleanup can legitimately originate another call.
            // Seal and acquire share AdmissionLease's atomic state, so a stale
            // retiring caller either precedes this fence and joins the drain,
            // or fails without touching a resource ledger.
            self.inner.runtime_admission.seal();
            self.inner.runtime_admission.wait_drained().await;
            self.inner.resources.wait_zero().await;
            let unresolved = self.unresolved_cleanup_report();
            if unresolved.total == 0 {
                // Runtime state and ShutdownRun are always locked in this
                // order. A terminal reason published before this fence stays
                // observable; a later one cannot mutate cached completion.
                let _state = self.inner.state.lock().expect("runtime state poisoned");
                self.inner.shutdown.finish();
            } else {
                self.inner.shutdown.mark_failed();
                self.mark_terminal_owned("runtime shutdown could not reach quiescence");
            }
        } else if !quiescent {
            self.inner.shutdown.mark_failed();
            self.mark_terminal_owned("runtime shutdown could not reach quiescence");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContractVersion, ProviderChannel, Provision, Requirement};
    use async_trait::async_trait;
    use std::time::Duration;

    const V1: ContractVersion = ContractVersion(1);

    #[derive(Debug)]
    struct Echo;

    #[async_trait]
    impl ServiceEndpoint for Echo {
        async fn serve(
            &self,
            _: InvocationContext,
            mut channel: ProviderChannel<'_>,
        ) -> Result<()> {
            while let Some(frame) = channel.recv().await {
                channel.send(frame).await?;
            }
            Ok(())
        }
    }

    #[derive(Debug)]
    struct ProviderFactory(PluginDescriptor);

    #[async_trait]
    impl PluginFactory for ProviderFactory {
        fn descriptor(&self) -> &PluginDescriptor {
            &self.0
        }

        async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
            context.provide("echo", "test.echo", V1, Arc::new(Echo))
        }
    }

    #[derive(Debug)]
    struct ConsumerFactory {
        descriptor: PluginDescriptor,
        captured: Arc<Mutex<Option<ServiceHandle>>>,
    }

    #[async_trait]
    impl PluginFactory for ConsumerFactory {
        fn descriptor(&self) -> &PluginDescriptor {
            &self.descriptor
        }

        async fn activate(&self, context: Context, _: Arc<Value>) -> Result<()> {
            *self.captured.lock().expect("service capture poisoned") =
                Some(context.service("echo")?);
            Ok(())
        }
    }

    async fn service_fixture() -> (Runtime, FiberHandle, FiberHandle, ServiceHandle) {
        let runtime = Runtime::default();
        let provider = runtime
            .root()
            .apply(
                Arc::new(ProviderFactory(
                    PluginDescriptor::new(FactoryIdentity::builtin("provider-seal", "1"))
                        .providing(Provision::new("echo", "test.echo", V1)),
                )),
                Value::Null,
            )
            .await
            .unwrap();
        let captured = Arc::new(Mutex::new(None));
        let consumer = runtime
            .root()
            .apply(
                Arc::new(ConsumerFactory {
                    descriptor: PluginDescriptor::new(FactoryIdentity::builtin(
                        "provider-seal-consumer",
                        "1",
                    ))
                    .requiring(Requirement::new("echo", "test.echo", V1)),
                    captured: Arc::clone(&captured),
                }),
                Value::Null,
            )
            .await
            .unwrap();
        let service = captured
            .lock()
            .expect("service capture poisoned")
            .clone()
            .expect("consumer captured its provider");
        (runtime, provider, consumer, service)
    }

    #[tokio::test]
    async fn provider_cleanup_hard_seals_stale_retiring_admission_before_draining() {
        let (_runtime, provider, consumer, service) = service_fixture().await;
        let generation_admission = Arc::clone(&service.binding.lease);
        let admitted_before_retirement = generation_admission
            .acquire(false)
            .expect("active provider admits an existing callback");
        let stale_retiring_consumer = true;

        let disposal = tokio::spawn(async move { provider.dispose().await });
        tokio::time::timeout(Duration::from_secs(1), async {
            while generation_admission
                .acquire(stale_retiring_consumer)
                .is_some()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("provider cleanup never hard-sealed retiring admission");
        assert!(
            !disposal.is_finished(),
            "cleanup bypassed an admission acquired before the seal"
        );

        drop(admitted_before_retirement);
        assert!(disposal.await.unwrap().is_clean());
        assert!(
            generation_admission
                .acquire(stale_retiring_consumer)
                .is_none()
        );
        assert!(consumer.dispose().await.is_clean());
    }

    #[tokio::test]
    async fn final_service_caller_check_rejects_a_generation_changed_after_initial_validation() {
        let (runtime, provider, consumer, service) = service_fixture().await;
        let owner = service.caller.owner.expect("captured service has an owner");
        let caller_fiber = runtime.owner_fiber(owner).unwrap();
        let retiring_consumer =
            Runtime::validate_service_caller(&service, owner, &caller_fiber).unwrap();
        assert!(!retiring_consumer);

        consumer.reconfigure(Value::Null).await.unwrap();
        assert!(matches!(
            Runtime::acquire_service_provider_lease(
                &service,
                owner,
                &caller_fiber,
                retiring_consumer,
            ),
            Err(MetaError::StaleContext { .. })
        ));
        tokio::time::timeout(Duration::from_secs(1), service.binding.lease.wait_drained())
            .await
            .expect("failed caller revalidation retained its provisional provider lease");

        assert!(consumer.dispose().await.is_clean());
        assert!(provider.dispose().await.is_clean());
    }

    #[tokio::test]
    async fn cached_shutdown_driver_failure_returns_without_another_wait_deadline() {
        let runtime = Runtime::default();
        runtime.inner.shutting_down.store(true, Ordering::Release);
        let mut report = CleanupReport::default();
        report.push_bounded(
            "runtime shutdown".to_owned(),
            "shutdown driver panicked",
            runtime.inner.limits.payloads.maximum_diagnostic_entries,
            runtime.inner.limits.payloads.maximum_diagnostic_bytes,
        );
        runtime.inner.shutdown.fail(
            report,
            runtime.inner.limits.payloads.maximum_diagnostic_entries,
            runtime.inner.limits.payloads.maximum_diagnostic_bytes,
        );

        let outcome = tokio::time::timeout(Duration::from_millis(20), runtime.shutdown())
            .await
            .expect("a cached driver failure waited for the shutdown deadline");
        let ShutdownOutcome::Failed { report, unresolved } = outcome else {
            panic!("cached driver failure returned a non-failure outcome");
        };
        assert_eq!(report.failures.len(), 1);
        assert_eq!(unresolved.total, 0);
    }
}
