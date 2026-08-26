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
                        .iter()
                        .map(|(slot, service)| (slot.clone(), Arc::clone(&service.binding)))
                        .collect::<Vec<_>>(),
                    listener_ids: active.listeners.keys().copied().collect::<BTreeSet<_>>(),
                    capabilities: Arc::clone(&active.capabilities),
                    lease: Arc::clone(&active.lease),
                    children: std::mem::take(&mut active.children),
                    retired_owned_report: std::mem::take(&mut active.retired_owned_report),
                    effects: {
                        let effects = std::mem::take(&mut active.effects);
                        for effect in effects.values() {
                            effect.claim_retirement();
                        }
                        effects.into_values().collect()
                    },
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
                fiber.executor.spawn(async move {
                    let cleanup = contain_panic_result(
                        std::panic::AssertUnwindSafe(
                            cleanup_runtime.with_reconciliation_slot(
                                cleanup_runtime
                                    .run_claimed_cleanup(Arc::clone(&cleanup_fiber), claimed),
                            ),
                        )
                        .catch_unwind()
                        .await,
                    );
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
            capabilities,
            lease,
            children,
            retired_owned_report,
            mut effects,
        } = claimed;
        let maximum_entries = self.inner.limits.payloads.maximum_diagnostic_entries;
        let maximum_bytes = self.inner.limits.payloads.maximum_diagnostic_bytes;
        let mut report = retired_owned_report;
        *fiber.cleanup_phase.lock().expect("cleanup phase poisoned") = CleanupPhase::Withdrawing;
        lease.close();
        self.yield_reconciliation_slot(self.revoke_capability_set(capabilities))
            .await;
        let reconciliation = self.withdraw_generation_services(&services, Some(fiber.id));
        let owner = Owner {
            fiber: fiber.id,
            generation,
        };
        // Listener withdrawal can await an in-flight callback and ultimately
        // destroy user code. Do not retain the provider's reconciliation slot
        // while exact dependents need that same bounded capacity to converge.
        self.yield_reconciliation_slot(async {
            for id in listener_ids {
                self.withdraw_listener_owned(owner, id).await;
            }
        })
        .await;
        *fiber.cleanup_phase.lock().expect("cleanup phase poisoned") =
            CleanupPhase::WaitingForDependents;
        self.join_reconciliation_requests(reconciliation).await;
        *fiber.cleanup_phase.lock().expect("cleanup phase poisoned") =
            CleanupPhase::DrainingAdmissions;
        // Ordinary holder admission is already fenced before dependent
        // convergence. Seal before the drain so no stale internal claimant can
        // reopen this generation after cleanup has observed zero callbacks.
        lease.seal();
        for (_, binding) in &services {
            binding.lease.seal();
        }
        self.yield_reconciliation_slot(async {
            lease.wait_drained().await;
            for (_, binding) in &services {
                binding.lease.wait_drained().await;
            }
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
                report.extend_bounded(
                    self.run_effect(&fiber, effect).await,
                    maximum_entries,
                    maximum_bytes,
                );
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
        services: &[(ServiceSlot, Arc<ProviderBinding>)],
        except: Option<FiberId>,
    ) -> Vec<(FiberId, ReconciliationTicket)> {
        let (tickets, should_spawn) = {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            let mut changed = Vec::new();
            let mut removed_any = false;
            for (slot, binding) in services {
                binding.lease.close();
                let visibility = state
                    .providers
                    .get(slot)
                    .filter(|entry| {
                        entry.binding.supply == binding.supply
                            && Arc::ptr_eq(&entry.binding, binding)
                    })
                    .map(|entry| entry.visibility);
                if visibility.is_some() {
                    state.providers.remove(slot);
                    removed_any = true;
                }
                if visibility == Some(SupplyVisibility::Active) {
                    changed.push(slot.clone());
                }
            }
            if removed_any {
                state.advance_revision();
            }
            let (tickets, should_spawn) =
                Self::request_service_withdrawals_locked(&mut state, &changed, except);
            (tickets, should_spawn)
        };
        self.start_reconciliation_requests(should_spawn);
        tickets
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
        let result = contain_panic_result(
            std::panic::AssertUnwindSafe(
                self.with_reconciliation_slot(self.dispose_fiber_owned(Arc::clone(&fiber))),
            )
            .catch_unwind()
            .await,
        );
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
        let required_slots = {
            let data = fiber.data.lock().expect("fiber state poisoned");
            let attempt = data
                .attempt
                .as_ref()
                .expect("registered Fiber retains its prepared attempt");
            attempt
                .required_services()
                .map(|service| fiber.base_context.service_slot(service))
                .collect::<Vec<_>>()
        };
        {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            state.fibers.remove(&id);
            state.reconciliations.remove_queued(id);
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
                        active.retired_owned_report.extend_bounded(
                            report.clone(),
                            self.inner.limits.payloads.maximum_diagnostic_entries,
                            self.inner.limits.payloads.maximum_diagnostic_bytes,
                        );
                    }
                }
            }
            state.advance_revision();
        }
        // Capacity belongs to registry membership, not to management handles
        // that may outlive disposal.
        let retired = {
            let mut data = fiber.data.lock().expect("fiber state poisoned");
            let retired = (
                data.fiber_reservation.take(),
                data.factory.take(),
                data.desired.take(),
                data.attempt.take(),
                data.replacement.take(),
            );
            data.last_attempt = None;
            retired
        };
        drop(retired);
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
            let executor = roots
                .first()
                .map_or_else(tokio::runtime::Handle::current, |root| {
                    root.executor.clone()
                });
            let runtime = self.clone();
            executor.spawn(async move {
                let result = contain_panic_result(
                    std::panic::AssertUnwindSafe(runtime.shutdown_inner(roots))
                        .catch_unwind()
                        .await,
                );
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
    use crate::{ContractVersion, PreparedActivation, ProviderChannel, Requirement};
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
    struct ProviderFactory;

    #[async_trait]
    impl PluginFactory for ProviderFactory {
        fn identity(&self) -> FactoryIdentity {
            FactoryIdentity::builtin("provider-seal", "1")
        }

        fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
            Ok(PreparedActivation::new(desired.clone()))
        }

        async fn activate(&self, plan: ActivationPlan) -> Result<()> {
            plan.context()
                .provide("echo", "test.echo", V1, Arc::new(Echo))?;
            Ok(())
        }
    }

    #[derive(Debug)]
    struct ConsumerFactory {
        captured: Arc<Mutex<Option<Capability>>>,
    }

    #[async_trait]
    impl PluginFactory for ConsumerFactory {
        fn identity(&self) -> FactoryIdentity {
            FactoryIdentity::builtin("provider-seal-consumer", "1")
        }

        fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
            Ok(
                PreparedActivation::new(desired.clone()).requiring(Requirement::new(
                    "echo",
                    "test.echo",
                    V1,
                )),
            )
        }

        async fn activate(&self, plan: ActivationPlan) -> Result<()> {
            *self.captured.lock().expect("service capture poisoned") =
                Some(plan.inject("echo").cloned().ok_or_else(|| {
                    MetaError::ServiceUnavailable {
                        service: ServiceKey::new("echo"),
                    }
                })?);
            Ok(())
        }
    }

    async fn service_fixture() -> (Runtime, FiberHandle, FiberHandle, Capability) {
        let runtime = Runtime::default();
        let provider = runtime
            .root()
            .apply(Arc::new(ProviderFactory), Value::Null)
            .await
            .unwrap();
        let captured = Arc::new(Mutex::new(None));
        let consumer = runtime
            .root()
            .apply(
                Arc::new(ConsumerFactory {
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
        let generation_admission = Arc::clone(&service.entry.binding.lease);
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
        let owner = service
            .holder
            .owner
            .expect("captured capability has an owner");
        let caller_fiber = runtime.owner_fiber(owner).unwrap();
        Runtime::validate_capability_holder(&service, owner, &caller_fiber).unwrap();
        let provisional_provider_lease = service
            .entry
            .binding
            .lease
            .acquire(false)
            .expect("active provider admits a provisional call");

        consumer.reconfigure(Value::Null).await.unwrap();
        assert!(matches!(
            Runtime::validate_capability_holder(&service, owner, &caller_fiber),
            Err(MetaError::StaleContext { .. })
        ));
        drop(provisional_provider_lease);
        tokio::time::timeout(
            Duration::from_secs(1),
            service.entry.binding.lease.wait_drained(),
        )
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
