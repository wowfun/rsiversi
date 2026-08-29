#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;
use crate::Requirement;
use crate::plugin::LocalRequirement;

impl Runtime {
    pub(super) fn reconcile_fiber(&self, fiber: Arc<Fiber>) -> BoxFuture<'static, ()> {
        let runtime = self.clone();
        Box::pin(async move {
            let reconciliation = contain_panic_result(
                std::panic::AssertUnwindSafe(runtime.reconcile_fiber_inner(&fiber))
                    .catch_unwind()
                    .await,
            );
            if reconciliation.is_err() {
                let published = fiber
                    .data
                    .lock()
                    .expect("fiber state poisoned")
                    .active
                    .as_ref()
                    .is_some_and(|active| active.published);
                let cleanup = contain_panic_result(if published {
                    std::panic::AssertUnwindSafe(runtime.unload_generation(&fiber))
                        .catch_unwind()
                        .await
                } else {
                    std::panic::AssertUnwindSafe(runtime.rollback_loading(&fiber))
                        .catch_unwind()
                        .await
                });
                let maximum = runtime.inner.limits.payloads.maximum_diagnostic_bytes;
                let message = match cleanup {
                    Ok(report) if report.is_clean() => "plugin activation panicked".to_owned(),
                    Ok(report) => diagnostics::bound_formatted(
                        format_args!(
                            "plugin activation panicked; cleanup also failed: {:?}",
                            report.failures
                        ),
                        maximum,
                    ),
                    Err(_) => "plugin activation and cleanup panicked".to_owned(),
                };
                fiber.set_state(FiberState::Failed(diagnostics::bound_owned(
                    message, maximum,
                )));
            }
        })
    }

    #[allow(clippy::too_many_lines)] // One reconciliation pass owns replacement, resolution, and activation ordering.
    pub(super) async fn reconcile_fiber_inner(&self, fiber: &Arc<Fiber>) {
        let (disposed, active_revision, target_revision) = {
            let data = fiber.data.lock().expect("fiber state poisoned");
            (
                data.disposed,
                data.active.as_ref().map(|active| active.target_revision),
                data.target_revision,
            )
        };
        if disposed {
            // Disposal owns teardown and its report. A concurrent reconciliation
            // must release the transition lock without consuming cleanup failures.
            return;
        }

        // A desired-configuration replacement is prepared and fully reserved
        // before the current generation retires. Only after cleanup has
        // completed does the fresh attempt become the dependency authority.
        if active_revision.is_some() && active_revision != Some(target_revision) {
            let cleanup = self.unload_generation(fiber).await;
            if !cleanup.is_clean() {
                fiber.set_state(FiberState::Failed(diagnostics::bound_formatted(
                    format_args!("reconfiguration cleanup failed: {:?}", cleanup.failures),
                    self.inner.limits.payloads.maximum_diagnostic_bytes,
                )));
                return;
            }
            self.promote_replacement_attempt(fiber);
        }

        let promote_after_cancelled_loading = {
            let data = fiber.data.lock().expect("fiber state poisoned");
            data.active.is_none() && data.replacement.is_some()
        };
        if promote_after_cancelled_loading {
            self.promote_replacement_attempt(fiber);
        }
        let needs_fresh_preparation = {
            let data = fiber.data.lock().expect("fiber state poisoned");
            data.active.is_none()
                && data.replacement.is_none()
                && data
                    .attempt
                    .as_ref()
                    .is_some_and(|attempt| attempt.consumed)
        };
        if needs_fresh_preparation {
            match self.refresh_consumed_attempt(fiber).await {
                Ok(true) => {}
                Ok(false) => return,
                Err(error) => {
                    fiber.set_state(FiberState::Failed(error));
                    return;
                }
            }
        }

        let (active_bindings, active_revision, target_revision, last_attempt) = {
            let data = fiber.data.lock().expect("fiber state poisoned");
            (
                data.active.as_ref().map(|active| {
                    binding_identities(&active.bindings, &active.local_bindings, fiber)
                }),
                data.active.as_ref().map(|active| active.target_revision),
                data.target_revision,
                data.last_attempt.clone(),
            )
        };

        let bindings = match self.resolve_bindings(fiber) {
            Ok(bindings) => bindings,
            Err(reasons) => {
                if let Some(active_bindings) = active_bindings.as_ref() {
                    self.replace_active_generation(fiber, active_bindings, "dependency retirement")
                        .await;
                    return;
                }
                fiber.set_state(FiberState::Pending(reasons));
                return;
            }
        };
        let next_bindings = binding_identities(&bindings.bindings, &bindings.local_bindings, fiber);
        let should_activate = {
            let data = fiber.data.lock().expect("fiber state poisoned");
            match (&data.state, active_bindings.as_ref()) {
                (FiberState::Active, Some(current)) => {
                    current != &next_bindings || active_revision != Some(target_revision)
                }
                (FiberState::Failed(_), _) => {
                    last_attempt.as_ref() != Some(&(target_revision, next_bindings.clone()))
                }
                (_, Some(current)) => current != &next_bindings,
                _ => true,
            }
        };
        if !should_activate {
            return;
        }
        if let Some(active_bindings) = active_bindings.as_ref() {
            self.replace_active_generation(fiber, active_bindings, "reconfiguration")
                .await;
            return;
        }
        self.activate_generation(fiber, bindings).await;
    }

    async fn replace_active_generation(
        &self,
        fiber: &Arc<Fiber>,
        active_bindings: &BindingIdentities,
        cleanup_operation: &'static str,
    ) {
        let prepared = match self.refresh_consumed_attempt(fiber).await {
            Ok(prepared) => prepared,
            Err(error) => {
                self.settle_replacement_preparation_failure(fiber, active_bindings, error)
                    .await;
                return;
            }
        };
        if !prepared {
            return;
        }

        let cleanup = self.unload_generation(fiber).await;
        if !cleanup.is_clean() {
            fiber.set_state(FiberState::Failed(diagnostics::bound_formatted(
                format_args!("{cleanup_operation} cleanup failed: {:?}", cleanup.failures),
                self.inner.limits.payloads.maximum_diagnostic_bytes,
            )));
            return;
        }

        self.promote_replacement_attempt(fiber);
        match self.resolve_bindings(fiber) {
            Ok(bindings) => self.activate_generation(fiber, bindings).await,
            Err(reasons) => fiber.set_state(FiberState::Pending(reasons)),
        }
    }

    pub(super) fn resolve_bindings(
        &self,
        fiber: &Fiber,
    ) -> std::result::Result<ResolvedBindings, PendingReport> {
        let (attempt, requirements, local_requirements) = {
            let data = fiber.data.lock().expect("fiber state poisoned");
            let attempt = data
                .attempt
                .as_ref()
                .expect("registered Fiber retains its prepared attempt");
            (
                AttemptStamp::from(attempt),
                Arc::clone(&attempt.requirements),
                Arc::clone(&attempt.local_requirements),
            )
        };
        let slots = requirements
            .iter()
            .enumerate()
            .map(|(index, requirement)| {
                let isolation =
                    Self::isolation_for(&fiber.base_context.isolation, &requirement.key);
                (
                    index,
                    isolation,
                    fiber.base_context.service_slot(&requirement.key),
                )
            })
            .collect::<Vec<_>>();
        let local_slots = local_requirements
            .iter()
            .enumerate()
            .map(|(index, requirement)| {
                (index, fiber.base_context.local_slot(requirement.contract))
            })
            .collect::<Vec<_>>();
        // One registry lock is one dependency snapshot. An activation must
        // never observe requirements from different publication revisions.
        let (candidates, local_candidates) = {
            let state = self.inner.state.lock().expect("runtime state poisoned");
            let portable = slots
                .iter()
                .map(|(_, _, slot)| {
                    state
                        .providers
                        .get(slot)
                        .filter(|entry| entry.visibility == SupplyVisibility::Active)
                        .map(|entry| Arc::clone(&entry.binding))
                })
                .collect::<Vec<_>>();
            let local = local_slots
                .iter()
                .map(|(_, slot)| {
                    state
                        .local_providers
                        .get(slot)
                        .filter(|entry| entry.visibility == SupplyVisibility::Active)
                        .map(|entry| Arc::clone(&entry.binding))
                })
                .collect::<Vec<_>>();
            (portable, local)
        };
        let mut pending = PendingReportBuilder::new(&self.inner.limits.payloads);
        let bindings = resolve_portable_candidates(&requirements, slots, candidates, &mut pending);
        let local_bindings = resolve_local_candidates(
            &local_requirements,
            local_slots,
            local_candidates,
            &mut pending,
        );
        if pending.total_reasons() == 0 {
            Ok(ResolvedBindings {
                attempt,
                bindings,
                local_bindings,
            })
        } else {
            Err(pending.finish())
        }
    }

    pub(super) fn replace_dependent_requirements(
        state: &mut RuntimeState,
        fiber: &Fiber,
        previous: &PreparedAttempt,
        next: &PreparedAttempt,
    ) {
        let previous_portable = previous
            .required_services()
            .map(|key| fiber.base_context.service_slot(key))
            .collect::<BTreeSet<_>>();
        let next_portable = next
            .required_services()
            .map(|key| fiber.base_context.service_slot(key))
            .collect::<BTreeSet<_>>();
        for slot in previous_portable.difference(&next_portable) {
            let remove_slot = state.dependents.get_mut(slot).is_some_and(|fibers| {
                fibers.remove(&fiber.id);
                fibers.is_empty()
            });
            if remove_slot {
                state.dependents.remove(slot);
            }
        }
        for slot in next_portable.difference(&previous_portable) {
            state
                .dependents
                .entry(slot.clone())
                .or_default()
                .insert(fiber.id);
        }

        let previous_local = previous
            .required_local_services()
            .map(|requirement| fiber.base_context.local_slot(requirement.contract))
            .collect::<BTreeSet<_>>();
        let next_local = next
            .required_local_services()
            .map(|requirement| fiber.base_context.local_slot(requirement.contract))
            .collect::<BTreeSet<_>>();
        for slot in previous_local.difference(&next_local) {
            let remove_slot = state.local_dependents.get_mut(slot).is_some_and(|fibers| {
                fibers.remove(&fiber.id);
                fibers.is_empty()
            });
            if remove_slot {
                state.local_dependents.remove(slot);
            }
        }
        for slot in next_local.difference(&previous_local) {
            state
                .local_dependents
                .entry(*slot)
                .or_default()
                .insert(fiber.id);
        }
    }

    pub(super) fn promote_replacement_attempt(&self, fiber: &Fiber) {
        let previous = {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            let mut data = fiber.data.lock().expect("fiber state poisoned");
            let Some(next) = data.replacement.take() else {
                return;
            };
            let previous = data
                .attempt
                .replace(next)
                .expect("registered Fiber retains its current prepared attempt");
            Runtime::replace_dependent_requirements(
                &mut state,
                fiber,
                &previous,
                data.attempt
                    .as_ref()
                    .expect("replacement attempt was promoted"),
            );
            data.last_attempt = None;
            previous
        };
        drop(previous);
    }

    #[allow(clippy::too_many_lines)] // Admission, blocking preparation, and fenced installation are one attempt transaction.
    pub(super) async fn refresh_consumed_attempt(
        &self,
        fiber: &Arc<Fiber>,
    ) -> std::result::Result<bool, String> {
        let (factory, desired, desired_revision, previous_id) = {
            let data = fiber.data.lock().expect("fiber state poisoned");
            let Some(previous) = data.attempt.as_ref() else {
                return Ok(false);
            };
            let desired = data
                .desired
                .as_ref()
                .expect("registered Fiber retains its desired configuration");
            (
                data.factory
                    .as_ref()
                    .expect("registered Fiber retains its factory")
                    .clone(),
                Arc::clone(&desired.value),
                desired.revision,
                previous.id,
            )
        };
        let runtime = self.clone();
        let prepared = self
            .yield_reconciliation_slot(async move {
                let (admission, reservations) = runtime.wait_for_attempt_preparation().await?;
                Ok::<_, MetaError>(
                    tokio::task::spawn_blocking(move || {
                        runtime.prepare_retained_attempt_admitted(
                            &factory,
                            &desired,
                            desired_revision,
                            admission,
                            reservations,
                        )
                    })
                    .await,
                )
            })
            .await;
        let prepared = match prepared {
            Ok(Ok(Ok(prepared))) => prepared,
            Err(error) | Ok(Ok(Err(error))) => {
                return Err(diagnostics::bound_formatted(
                    format_args!("plugin preparation failed: {error}"),
                    self.inner.limits.payloads.maximum_diagnostic_bytes,
                ));
            }
            Ok(Err(error)) => {
                return Err(diagnostics::bound_formatted(
                    format_args!("plugin preparation task failed: {error}"),
                    self.inner.limits.payloads.maximum_diagnostic_bytes,
                ));
            }
        };
        let mut incoming = Some(prepared);
        let (accepted, retired) = {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            let mut data = fiber.data.lock().expect("fiber state poisoned");
            let current = data
                .attempt
                .as_ref()
                .expect("registered Fiber retains its prepared attempt");
            if data.disposed
                || current.id != previous_id
                || data
                    .desired
                    .as_ref()
                    .is_none_or(|desired| desired.revision != desired_revision)
            {
                (false, incoming.take())
            } else if data.active.is_some() {
                let previous = data.replacement.replace(
                    incoming
                        .take()
                        .expect("fresh attempt is installed at most once"),
                );
                (true, previous)
            } else {
                let previous = data
                    .attempt
                    .replace(
                        incoming
                            .take()
                            .expect("fresh attempt is installed at most once"),
                    )
                    .expect("registered Fiber retains its previous attempt");
                Runtime::replace_dependent_requirements(
                    &mut state,
                    fiber,
                    &previous,
                    data.attempt.as_ref().expect("fresh attempt was installed"),
                );
                data.last_attempt = None;
                if matches!(data.state, FiberState::Failed(_)) {
                    data.state = FiberState::Pending(PendingReport::default());
                }
                (true, Some(previous))
            }
        };
        drop(retired);
        Ok(accepted)
    }

    pub(super) async fn settle_replacement_preparation_failure(
        &self,
        fiber: &Arc<Fiber>,
        active_bindings: &BindingIdentities,
        preparation_error: String,
    ) {
        let active_binding_is_still_current = self.resolve_bindings(fiber).is_ok_and(|bindings| {
            binding_identities(&bindings.bindings, &bindings.local_bindings, fiber)
                == *active_bindings
        });
        if active_binding_is_still_current {
            // A registry race can make a refresh attempt obsolete while the
            // already-published generation is still authoritative. Keep that
            // generation Active; a later registry intent can retry freshly.
            return;
        }

        let cleanup = self.unload_generation(fiber).await;
        let error = if cleanup.is_clean() {
            preparation_error
        } else {
            diagnostics::bound_formatted(
                format_args!(
                    "{preparation_error}; dependency retirement cleanup also failed: {:?}",
                    cleanup.failures
                ),
                self.inner.limits.payloads.maximum_diagnostic_bytes,
            )
        };
        fiber.set_state(FiberState::Failed(error));
    }
}

fn resolve_portable_candidates(
    requirements: &[Requirement],
    slots: Vec<(usize, IsolationId, ServiceSlot)>,
    candidates: Vec<Option<Arc<ProviderBinding>>>,
    pending: &mut PendingReportBuilder,
) -> BTreeMap<ServiceKey, Arc<ProviderBinding>> {
    let mut bindings = BTreeMap::new();
    for ((index, isolation, _), binding) in slots.into_iter().zip(candidates) {
        let requirement = &requirements[index];
        let Some(binding) = binding else {
            pending.push_with(1, requirement.key.as_str().len(), || {
                PendingReason::MissingService {
                    service: requirement.key.clone(),
                    isolation,
                }
            });
            continue;
        };
        if binding.contract != requirement.contract || binding.version != requirement.version {
            let retained_bytes = requirement
                .key
                .as_str()
                .len()
                .saturating_add(requirement.contract.as_str().len())
                .saturating_add(binding.contract.as_str().len());
            pending.push_with(1, retained_bytes, || PendingReason::ContractMismatch {
                service: requirement.key.clone(),
                expected: requirement.contract.clone(),
                expected_version: requirement.version,
                actual: binding.contract.clone(),
                actual_version: binding.version,
            });
            continue;
        }
        bindings.insert(requirement.key.clone(), binding);
    }
    bindings
}

fn resolve_local_candidates(
    requirements: &[LocalRequirement],
    slots: Vec<(usize, LocalSlot)>,
    candidates: Vec<Option<Arc<LocalBinding>>>,
    pending: &mut PendingReportBuilder,
) -> BTreeMap<TypeId, Arc<LocalBinding>> {
    let mut bindings = BTreeMap::new();
    for ((index, slot), binding) in slots.into_iter().zip(candidates) {
        let requirement = &requirements[index];
        let Some(binding) = binding else {
            pending.push_with(1, requirement.key.as_str().len(), || {
                PendingReason::MissingLocal {
                    contract: requirement.key.clone(),
                    isolation: slot.isolation,
                }
            });
            continue;
        };
        bindings.insert(requirement.contract, binding);
    }
    bindings
}
