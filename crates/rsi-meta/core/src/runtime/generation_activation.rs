#![allow(clippy::wildcard_imports)] // This is the generation-activation ownership partition.

use super::*;

impl Runtime {
    #[allow(clippy::too_many_lines)] // Activation, rollback, and publication are one generation transaction.
    pub(super) async fn activate_generation(&self, fiber: &Arc<Fiber>, resolved: ResolvedBindings) {
        let ResolvedBindings {
            attempt: resolved_attempt,
            bindings,
            local_bindings,
        } = resolved;
        let generation = match self.next_generation_id() {
            Ok(generation) => generation,
            Err(error) => {
                fiber.set_state(FiberState::Failed(diagnostics::bound_formatted(
                    format_args!("{error}"),
                    self.inner.limits.payloads.maximum_diagnostic_bytes,
                )));
                return;
            }
        };
        let activation_lineage = match self.next_call_id() {
            Ok(lineage) => lineage,
            Err(error) => {
                fiber.set_state(FiberState::Failed(diagnostics::bound_formatted(
                    format_args!("{error}"),
                    self.inner.limits.payloads.maximum_diagnostic_bytes,
                )));
                return;
            }
        };
        let activation_cancellation = CancellationToken::new();
        let resolved_bindings = bindings.clone();
        let resolved_local_bindings = local_bindings.clone();
        let installed = {
            let state = self.inner.state.lock().expect("runtime state poisoned");
            if Self::bindings_remain_active(&state, fiber, &bindings, &local_bindings) {
                let mut data = fiber.data.lock().expect("fiber state poisoned");
                let current_attempt = data.attempt.as_ref().map(AttemptStamp::from);
                if resolved_attempt.permits_loading_install(
                    current_attempt,
                    data.target_revision,
                    data.disposed,
                    fiber.disposal_requested.is_cancelled(),
                ) {
                    let target_revision = data.target_revision;
                    let attempt = binding_identities(&bindings, &local_bindings, fiber);
                    let prepared = data
                        .attempt
                        .as_mut()
                        .expect("registered Fiber retains its prepared attempt");
                    debug_assert_eq!(prepared.desired_revision, target_revision);
                    debug_assert!(!prepared.consumed, "one attempt may activate only once");
                    prepared.consumed = true;
                    let attempt_id = prepared.id;
                    let prepared_state = prepared.state.take();
                    data.generation = generation;
                    data.state = FiberState::Loading;
                    data.active = Some(GenerationData {
                        generation,
                        attempt_id,
                        bindings,
                        local_bindings,
                        activation_cancellation: activation_cancellation.clone(),
                        effects: BTreeMap::new(),
                        effect_budget: Arc::new(GenerationBudget::new(
                            self.inner.limits.topology.maximum_effects_per_fiber,
                        )),
                        effect_transaction_budget: Arc::new(GenerationBudget::new(
                            self.inner
                                .limits
                                .topology
                                .maximum_effect_transactions_per_fiber,
                        )),
                        services: BTreeMap::new(),
                        local_services: BTreeMap::new(),
                        local_listener_ids: BTreeMap::new(),
                        children: Vec::new(),
                        retired_owned_report: CleanupReport::default(),
                        cleanup: Arc::new(CleanupRun::default()),
                        capabilities: GenerationCapabilitySet::new(),
                        lease: Arc::new(AdmissionLease::default()),
                        published: false,
                        target_revision,
                    });
                    data.last_attempt = Some((target_revision, attempt));
                    let snapshot = data.snapshot(fiber.id);
                    fiber.watch.send_replace(snapshot);
                    let capabilities = Arc::clone(
                        &data
                            .active
                            .as_ref()
                            .expect("loading generation exists")
                            .capabilities,
                    );
                    Some((
                        data.factory
                            .as_ref()
                            .expect("registered Fiber retains its factory")
                            .clone(),
                        Arc::clone(
                            &data
                                .attempt
                                .as_ref()
                                .expect("registered Fiber retains its prepared attempt")
                                .config,
                        ),
                        prepared_state,
                        capabilities,
                        data.target_revision,
                    ))
                } else {
                    None
                }
            } else {
                None
            }
        };
        let Some((factory, config, prepared_state, capabilities, target_revision)) = installed
        else {
            // A provider, attempt, desired revision, or disposal state changed
            // after resolution but before Loading had a cancellation token to
            // fence it. Coalesce a fresh pass explicitly; installation must
            // never rely on notification timing alone.
            let _ticket = self.request_reconciliation(fiber.id);
            return;
        };
        let owner = Owner {
            fiber: fiber.id,
            generation,
        };
        let activation_label = diagnostics::bound_owned(
            "plugin activation root".to_owned(),
            self.inner.limits.payloads.maximum_diagnostic_bytes,
        );
        let mut activation_transaction = match self.begin_effect(owner, activation_label) {
            Ok(transaction) => transaction,
            Err(error) => {
                let cleanup = self.rollback_loading(fiber).await;
                let error = if cleanup.is_clean() {
                    diagnostics::bound_formatted(
                        format_args!("activation transaction failed: {error}"),
                        self.inner.limits.payloads.maximum_diagnostic_bytes,
                    )
                } else {
                    diagnostics::bound_formatted(
                        format_args!(
                            "activation transaction failed: {error}; rollback failed: {:?}",
                            cleanup.failures
                        ),
                        self.inner.limits.payloads.maximum_diagnostic_bytes,
                    )
                };
                fiber.set_state(FiberState::Failed(error));
                return;
            }
        };
        // This oldest record belongs to the complete generation, not to the
        // activation stack frame. If plugin code unwinds, outer generation
        // rollback must dispose children and later effects before this root.
        activation_transaction.defer_drop_to_generation_rollback();
        let setup_effect = activation_transaction.scope();
        let mut context = fiber.context(generation);
        context.install_activation_lineage(fiber.id, activation_lineage);
        context.setup_effect = Some(setup_effect);
        let inject = resolved_bindings
            .into_iter()
            .map(|(key, binding)| {
                self.mint_capability(&context, owner, &capabilities, binding)
                    .map(|capability| (key, capability))
            })
            .collect::<Result<BTreeMap<_, _>>>();
        // Plugin code may synchronously await another scheduler-backed
        // operation through a service or a spawned task. It owns no registry
        // mutation while awaiting, so transfer the slot until its result is
        // ready and reacquire before publication or rollback.
        let result = match inject {
            Ok(inject) => {
                let plan = ActivationPlan::new(
                    context,
                    Arc::clone(&config.value),
                    inject,
                    resolved_local_bindings,
                    prepared_state,
                );
                let deadline = tokio::time::Instant::now()
                    .checked_add(self.inner.limits.deadlines.transition)
                    .expect("validated activation deadline fits Tokio Instant");
                self.yield_reconciliation_slot(
                    activation_driver::ActivationDriver {
                        factory: &factory,
                        plan,
                        apply_cancellation: &fiber.apply_cancellation,
                        generation_cancellation: &activation_cancellation,
                        deadline,
                    }
                    .run(),
                )
                .await
            }
            Err(error) => {
                drop_catching_unwind(prepared_state);
                Err(error)
            }
        };
        // Keep the normalized-configuration reservation alive through plugin
        // future and opaque-state destruction.
        drop(config);
        drop(factory);
        let maximum = self.inner.limits.payloads.maximum_diagnostic_bytes;
        let activation_error = match result {
            Ok(()) => None,
            Err(error) => Some(diagnostics::bound_formatted(
                format_args!("{error}"),
                maximum,
            )),
        };
        if let Some(error) = activation_error {
            // Close setup without starting an independent cleanup driver.
            // Generation rollback owns the one reverse-ordered transaction and
            // its complete report.
            activation_transaction.close_for_runtime_rollback();
            let cleanup = self.rollback_loading(fiber).await;
            let error = if cleanup.is_clean() {
                error
            } else {
                diagnostics::bound_formatted(
                    format_args!(
                        "{error}; activation rollback failed: {:?}",
                        cleanup.failures
                    ),
                    maximum,
                )
            };
            fiber.set_state(FiberState::Failed(error));
            return;
        }

        let activation_root = match activation_transaction.commit() {
            Ok(handle) => handle,
            Err(error) => {
                let cleanup = self.rollback_loading(fiber).await;
                let error = if cleanup.is_clean() {
                    diagnostics::bound_formatted(
                        format_args!("activation transaction commit failed: {error}"),
                        maximum,
                    )
                } else {
                    diagnostics::bound_formatted(
                        format_args!(
                            "activation transaction commit failed: {error}; rollback failed: {:?}",
                            cleanup.failures
                        ),
                        maximum,
                    )
                };
                fiber.set_state(FiberState::Failed(error));
                return;
            }
        };
        // A passive setup has nothing to retain. A root that actually owns
        // setup effects remains generation-owned until retirement.
        if activation_root.is_empty() {
            let activation_root_report = activation_root.dispose().await;
            debug_assert!(activation_root_report.is_clean());
        }

        if let Err(error) = self.publish_generation(fiber, generation, target_revision) {
            let cleanup = self.rollback_loading(fiber).await;
            let error = if cleanup.is_clean() {
                diagnostics::bound_formatted(format_args!("{error}"), maximum)
            } else {
                diagnostics::bound_formatted(
                    format_args!(
                        "{error}; publication rollback failed: {:?}",
                        cleanup.failures
                    ),
                    maximum,
                )
            };
            fiber.set_state(FiberState::Failed(error));
        }
    }

    fn publish_generation(
        &self,
        fiber: &Arc<Fiber>,
        generation: FiberGeneration,
        target_revision: u64,
    ) -> Result<()> {
        let mut state = self.inner.state.lock().expect("runtime state poisoned");
        if let Some(reason) = state.terminal.clone() {
            return Err(MetaError::RuntimeTerminal(reason));
        }
        let mut data = fiber.data.lock().expect("fiber state poisoned");
        if data.disposed
            || fiber.disposal_requested.is_cancelled()
            || data.generation != generation
            || data.target_revision != target_revision
        {
            return Err(MetaError::StaleContext {
                fiber: fiber.id,
                generation,
            });
        }
        let (services, local_services) = {
            let active = data.active.as_ref().ok_or(MetaError::StaleContext {
                fiber: fiber.id,
                generation,
            })?;
            let exact_attempt = data.attempt.as_ref().is_some_and(|attempt| {
                attempt.id == active.attempt_id
                    && attempt.desired_revision == target_revision
                    && attempt.consumed
            });
            if !exact_attempt {
                return Err(MetaError::StaleContext {
                    fiber: fiber.id,
                    generation,
                });
            }
            if active.activation_cancellation.is_cancelled() {
                return Err(MetaError::Cancelled);
            }
            if !Self::bindings_remain_active(
                &state,
                fiber,
                &active.bindings,
                &active.local_bindings,
            ) {
                return Err(MetaError::StaleContext {
                    fiber: fiber.id,
                    generation,
                });
            }
            let portable = active
                .services
                .iter()
                .map(|(slot, service)| (slot.clone(), Arc::clone(&service.binding)))
                .collect::<Vec<_>>();
            let local = active
                .local_services
                .iter()
                .map(|(slot, service)| (*slot, Arc::clone(&service.binding)))
                .collect::<Vec<_>>();
            (portable, local)
        };
        Self::activate_loading_supplies(
            &mut state,
            &services,
            &local_services,
            fiber.id,
            generation,
        )?;
        data.active
            .as_mut()
            .expect("active generation exists")
            .published = true;
        data.last_attempt = None;
        data.state = FiberState::Active;
        let snapshot = data.snapshot(fiber.id);
        fiber.watch.send_replace(snapshot);
        state.advance_revision();
        let changed = services
            .into_iter()
            .map(|(slot, _binding)| slot)
            .collect::<Vec<_>>();
        let local_changed = local_services
            .into_iter()
            .map(|(slot, _binding)| slot)
            .collect::<Vec<_>>();
        drop(data);
        drop(state);
        self.notify_service_appearances(&changed, Some(fiber.id));
        self.notify_local_appearances(&local_changed, Some(fiber.id));
        Ok(())
    }

    fn activate_loading_supplies(
        state: &mut RuntimeState,
        services: &[(ServiceSlot, Arc<ProviderBinding>)],
        local_services: &[(LocalSlot, Arc<LocalBinding>)],
        fiber: FiberId,
        generation: FiberGeneration,
    ) -> Result<()> {
        let stale_context = || MetaError::StaleContext { fiber, generation };
        for (slot, binding) in services {
            let exact_loading = state.providers.get(slot).is_some_and(|entry| {
                entry.visibility == SupplyVisibility::Loading
                    && entry.binding.supply == binding.supply
                    && Arc::ptr_eq(&entry.binding, binding)
            });
            if !exact_loading {
                return Err(stale_context());
            }
        }
        for (slot, binding) in local_services {
            let exact_loading = state.local_providers.get(slot).is_some_and(|entry| {
                entry.visibility == SupplyVisibility::Loading
                    && entry.binding.supply == binding.supply
                    && Arc::ptr_eq(&entry.binding, binding)
            });
            if !exact_loading {
                return Err(stale_context());
            }
        }
        for (slot, binding) in services {
            let entry = state
                .providers
                .get_mut(slot)
                .expect("validated Loading supply remains present under the registry lock");
            debug_assert!(Arc::ptr_eq(&entry.binding, binding));
            entry.visibility = SupplyVisibility::Active;
        }
        for (slot, binding) in local_services {
            let entry = state
                .local_providers
                .get_mut(slot)
                .expect("validated Loading Local supply remains present under the registry lock");
            debug_assert!(Arc::ptr_eq(&entry.binding, binding));
            entry.visibility = SupplyVisibility::Active;
        }
        Ok(())
    }

    fn bindings_remain_active(
        state: &RuntimeState,
        fiber: &Fiber,
        bindings: &BTreeMap<ServiceKey, Arc<ProviderBinding>>,
        local_bindings: &BTreeMap<TypeId, Arc<LocalBinding>>,
    ) -> bool {
        let portable = bindings.iter().all(|(key, binding)| {
            let slot = fiber.base_context.service_slot(key);
            state.providers.get(&slot).is_some_and(|entry| {
                entry.visibility == SupplyVisibility::Active
                    && entry.binding.supply == binding.supply
                    && Arc::ptr_eq(&entry.binding, binding)
            })
        });
        portable
            && local_bindings.iter().all(|(contract, binding)| {
                let slot = fiber.base_context.local_slot(*contract);
                state.local_providers.get(&slot).is_some_and(|entry| {
                    entry.visibility == SupplyVisibility::Active
                        && entry.binding.supply == binding.supply
                        && Arc::ptr_eq(&entry.binding, binding)
                })
            })
    }
}

#[cfg(test)]
mod loading_install_tests {
    use super::*;

    #[test]
    fn loading_install_requires_the_exact_live_resolved_attempt() {
        let resolved = AttemptStamp {
            id: 7,
            desired_revision: 11,
            consumed: false,
        };
        assert!(resolved.permits_loading_install(Some(resolved), 11, false, false));

        for rejected in [
            resolved.permits_loading_install(None, 11, false, false),
            resolved.permits_loading_install(
                Some(AttemptStamp {
                    id: 8,
                    desired_revision: 11,
                    consumed: false,
                }),
                11,
                false,
                false,
            ),
            resolved.permits_loading_install(
                Some(AttemptStamp {
                    id: 7,
                    desired_revision: 12,
                    consumed: false,
                }),
                11,
                false,
                false,
            ),
            resolved.permits_loading_install(Some(resolved), 12, false, false),
            resolved.permits_loading_install(Some(resolved), 11, true, false),
            resolved.permits_loading_install(Some(resolved), 11, false, true),
            resolved.permits_loading_install(
                Some(AttemptStamp {
                    consumed: true,
                    ..resolved
                }),
                11,
                false,
                false,
            ),
        ] {
            assert!(!rejected);
        }
    }
}
