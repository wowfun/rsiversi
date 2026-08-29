#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) struct ServiceSlot {
    pub(super) key: ServiceKey,
    pub(super) isolation: IsolationId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SupplyVisibility {
    Loading,
    Active,
}

pub(super) struct SupplyEntry {
    pub(super) binding: Arc<ProviderBinding>,
    pub(super) visibility: SupplyVisibility,
}

/// Cloneable generation-fenced ownership handle for one dynamic service supply.
#[derive(Clone)]
pub struct SupplyHandle {
    id: SupplyId,
    key: ServiceKey,
    effect: SupplyEffect,
}

#[derive(Clone)]
enum SupplyEffect {
    Setup {
        disposal: Arc<SupplyDisposal>,
        effect: OwnedEffect,
    },
    Dynamic(EffectHandle),
}

struct SupplyDisposal {
    runtime: Weak<RuntimeInner>,
    owner: Owner,
    slot: ServiceSlot,
    binding: Arc<ProviderBinding>,
    label: String,
    executor: tokio::runtime::Handle,
    maximum_diagnostic_entries: usize,
    maximum_diagnostic_bytes: usize,
    started: AtomicBool,
    result: Mutex<Option<std::result::Result<(), String>>>,
    complete: Notify,
}

impl SupplyHandle {
    /// Returns the exact non-repeating supply identity.
    pub fn id(&self) -> SupplyId {
        self.id
    }

    /// Returns the logical service key occupied by this supply.
    pub fn key(&self) -> &ServiceKey {
        &self.key
    }

    /// Withdraws this exact supply and joins dependent convergence and call drain.
    ///
    /// Once this future is first polled, withdrawal remains Runtime-owned if
    /// the caller drops the future.
    pub async fn dispose(&self) -> CleanupReport {
        match &self.effect {
            SupplyEffect::Setup { disposal, effect } => disposal.dispose(effect).await,
            SupplyEffect::Dynamic(effect) => effect.dispose().await,
        }
    }
}

impl fmt::Debug for SupplyHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SupplyHandle")
            .field("id", &self.id)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl SupplyDisposal {
    fn new(
        runtime: &Runtime,
        owner: Owner,
        slot: ServiceSlot,
        binding: Arc<ProviderBinding>,
        label: String,
        executor: tokio::runtime::Handle,
    ) -> Arc<Self> {
        let maximum_diagnostic_entries = runtime.inner.limits.payloads.maximum_diagnostic_entries;
        let maximum_diagnostic_bytes = runtime.inner.limits.payloads.maximum_diagnostic_bytes;
        Arc::new(Self {
            runtime: Arc::downgrade(&runtime.inner),
            owner,
            slot,
            binding,
            label,
            executor,
            maximum_diagnostic_entries,
            maximum_diagnostic_bytes,
            started: AtomicBool::new(false),
            result: Mutex::new(None),
            complete: Notify::new(),
        })
    }

    fn cleanup(self: &Arc<Self>) -> Cleanup {
        let disposal = Arc::clone(self);
        Box::new(move || {
            async move {
                disposal.start_from_effect();
                disposal.join().await
            }
            .boxed()
        })
    }

    fn start_from_effect(self: &Arc<Self>) {
        if self.claim_start() {
            self.spawn(None);
        }
    }

    fn start_explicit(self: &Arc<Self>, effect: &OwnedEffect) {
        if self.claim_start() {
            // Detach only after winning the persistent cleanup run. If
            // retirement already moved the entry into its driver, `None`
            // means that driver still retains and reports this same cleanup.
            self.spawn(effect.detach());
        }
    }

    fn claim_start(&self) -> bool {
        self.started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn spawn(self: &Arc<Self>, retention: Option<EffectRetention>) {
        let Some(inner) = self.runtime.upgrade() else {
            // Once the last Runtime owner is gone, its registry and dormant
            // effect record are already being destroyed together. There is no
            // live registry authority left to reconcile.
            drop(retention);
            self.finish(Ok(()));
            return;
        };
        let runtime = Runtime { inner };
        let disposal = Arc::clone(self);
        self.executor.spawn(async move {
            let outcome = contain_panic_result(
                std::panic::AssertUnwindSafe(runtime.withdraw_supply(
                    disposal.owner,
                    disposal.slot.clone(),
                    Arc::clone(&disposal.binding),
                ))
                .catch_unwind()
                .await,
            );
            let result = if let Ok(result) = outcome {
                result
            } else {
                runtime.mark_terminal_owned("dynamic service supply cleanup driver panicked");
                Err("dynamic service supply cleanup driver panicked".to_owned())
            };
            let detached = retention.is_some();
            // A detached effect still owns its resource reservation through
            // the whole asynchronous withdrawal. Release it before waking a
            // waiter so a completed dispose implies accurate accounting.
            drop(retention);
            if detached {
                runtime.retain_detached_supply_result(disposal.owner, &disposal.label, &result);
            }
            disposal.finish(result);
        });
    }

    fn finish(&self, result: std::result::Result<(), String>) {
        let mut stored = self.result.lock().expect("supply disposal poisoned");
        if stored.is_none() {
            *stored = Some(result);
            self.complete.notify_waiters();
        }
    }

    async fn join(&self) -> std::result::Result<(), String> {
        loop {
            let notified = self.complete.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(result) = self
                .result
                .lock()
                .expect("supply disposal poisoned")
                .clone()
            {
                return result;
            }
            notified.as_mut().await;
        }
    }

    async fn dispose(self: &Arc<Self>, effect: &OwnedEffect) -> CleanupReport {
        self.start_explicit(effect);
        let result = self.join().await;
        let mut report = CleanupReport::default();
        if let Err(error) = result {
            report.push_bounded(
                self.label.clone(),
                error,
                self.maximum_diagnostic_entries,
                self.maximum_diagnostic_bytes,
            );
        }
        report
    }
}

impl Runtime {
    pub(super) fn provide(
        &self,
        context: &Context,
        key: ServiceKey,
        contract: ContractId,
        version: ContractVersion,
        endpoint: Arc<dyn ServiceEndpoint>,
    ) -> Result<SupplyHandle> {
        let (supply, capability) =
            self.provide_inner(context, key, contract, version, endpoint, false)?;
        debug_assert!(capability.is_none());
        Ok(supply)
    }

    pub(super) fn provide_and_capture(
        &self,
        context: &Context,
        key: ServiceKey,
        contract: ContractId,
        version: ContractVersion,
        endpoint: Arc<dyn ServiceEndpoint>,
    ) -> Result<(SupplyHandle, Capability)> {
        let (supply, capability) =
            self.provide_inner(context, key, contract, version, endpoint, true)?;
        Ok((
            supply,
            capability.expect("capture mode returns one capability"),
        ))
    }

    fn provide_inner(
        &self,
        context: &Context,
        key: ServiceKey,
        contract: ContractId,
        version: ContractVersion,
        endpoint: Arc<dyn ServiceEndpoint>,
        capture: bool,
    ) -> Result<(SupplyHandle, Option<Capability>)> {
        let _runtime_admission = self.begin_admission(false)?;
        let owner = context.owner.ok_or_else(|| {
            MetaError::InvalidInput("the root context cannot provide a service".to_owned())
        })?;
        let token = self
            .inner
            .next_supply
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map_err(|_| MetaError::CapacityExhausted {
                resource: "supply identities",
            })?
            + 1;
        let id = SupplyId::new(owner.fiber, owner.generation, token);
        let slot = ServiceSlot {
            key: key.clone(),
            isolation: Self::isolation_for(&context.isolation, &key),
        };
        let binding = Arc::new(ProviderBinding {
            supply: id,
            key: key.clone(),
            contract,
            version,
            provider: owner.fiber,
            generation: owner.generation,
            endpoint: Mutex::new(Some(endpoint)),
            lease: Arc::new(AdmissionLease::default()),
        });

        // The cleanup wrapper exists before the registry can observe the
        // supply. If insertion or commit races retirement, the same exact
        // record removes any successfully inserted binding.
        let maximum_diagnostic_bytes = self.inner.limits.payloads.maximum_diagnostic_bytes;
        let transaction_label = diagnostics::bound_owned(
            "dynamic service supply".to_owned(),
            maximum_diagnostic_bytes,
        );
        let cleanup_label = diagnostics::bound_owned(
            "withdraw dynamic service supply".to_owned(),
            maximum_diagnostic_bytes,
        );
        let fiber = self.owner_fiber(owner)?;
        let capability = if capture {
            let capabilities = {
                let data = fiber.data.lock().expect("fiber state poisoned");
                if data.generation != owner.generation
                    || !matches!(data.state, FiberState::Loading | FiberState::Active)
                {
                    return Err(MetaError::StaleContext {
                        fiber: owner.fiber,
                        generation: owner.generation,
                    });
                }
                Arc::clone(
                    &data
                        .active
                        .as_ref()
                        .ok_or(MetaError::StaleContext {
                            fiber: owner.fiber,
                            generation: owner.generation,
                        })?
                        .capabilities,
                )
            };
            Some(self.mint_capability(context, owner, &capabilities, Arc::clone(&binding))?)
        } else {
            None
        };
        let disposal = SupplyDisposal::new(
            self,
            owner,
            slot.clone(),
            Arc::clone(&binding),
            cleanup_label.clone(),
            fiber.executor.clone(),
        );

        let effect = if let Some(setup) = context
            .setup_effect
            .as_ref()
            .filter(|setup| setup.is_open())
        {
            self.attach_setup_supply(setup, owner, &slot, &binding, disposal, cleanup_label)?
        } else {
            self.publish_dynamic_supply(
                owner,
                &slot,
                &binding,
                &disposal,
                transaction_label,
                cleanup_label,
            )?
        };

        Ok((SupplyHandle { id, key, effect }, capability))
    }

    fn attach_setup_supply(
        &self,
        setup: &EffectScope,
        owner: Owner,
        slot: &ServiceSlot,
        binding: &Arc<ProviderBinding>,
        disposal: Arc<SupplyDisposal>,
        cleanup_label: String,
    ) -> Result<SupplyEffect> {
        let effect = setup.defer_owned(cleanup_label, disposal.cleanup())?;
        let visible = match self.register_supply(owner, slot, binding) {
            Ok(visible) => visible,
            Err(error) => {
                // The supply never entered the registry, so remove its
                // unpublished undo immediately. If retirement claimed the
                // root first, that same root already owns the cleanup.
                drop(effect.detach());
                return Err(error);
            }
        };
        debug_assert!(!visible, "an open activation root implies Loading state");
        Ok(SupplyEffect::Setup { disposal, effect })
    }

    fn publish_dynamic_supply(
        &self,
        owner: Owner,
        slot: &ServiceSlot,
        binding: &Arc<ProviderBinding>,
        disposal: &Arc<SupplyDisposal>,
        transaction_label: String,
        cleanup_label: String,
    ) -> Result<SupplyEffect> {
        let mut transaction = self.begin_effect(owner, transaction_label)?;
        transaction.defer(cleanup_label, disposal.cleanup())?;
        let visible = self.register_supply(owner, slot, binding)?;
        if visible {
            self.notify_service_appearances(std::slice::from_ref(slot), Some(owner.fiber));
        }
        Ok(SupplyEffect::Dynamic(transaction.commit()?))
    }

    fn retain_detached_supply_result(
        &self,
        owner: Owner,
        label: &str,
        result: &std::result::Result<(), String>,
    ) {
        let Err(error) = result else {
            return;
        };
        let Ok(fiber) = self.owner_fiber(owner) else {
            return;
        };
        let mut data = fiber.data.lock().expect("fiber state poisoned");
        if data.generation != owner.generation {
            return;
        }
        let Some(active) = data.active.as_mut() else {
            return;
        };
        let mut report = CleanupReport::default();
        report.push_bounded(
            label.to_owned(),
            error,
            self.inner.limits.payloads.maximum_diagnostic_entries,
            self.inner.limits.payloads.maximum_diagnostic_bytes,
        );
        active.retired_owned_report.extend_bounded(
            report,
            self.inner.limits.payloads.maximum_diagnostic_entries,
            self.inner.limits.payloads.maximum_diagnostic_bytes,
        );
    }

    fn register_supply(
        &self,
        owner: Owner,
        slot: &ServiceSlot,
        binding: &Arc<ProviderBinding>,
    ) -> Result<bool> {
        let mut state = self.inner.state.lock().expect("runtime state poisoned");
        let fiber = state
            .fibers
            .get(&owner.fiber)
            .cloned()
            .ok_or(MetaError::StaleContext {
                fiber: owner.fiber,
                generation: owner.generation,
            })?;
        let mut data = fiber.data.lock().expect("fiber state poisoned");
        if data.generation != owner.generation
            || !matches!(data.state, FiberState::Loading | FiberState::Active)
        {
            return Err(MetaError::StaleContext {
                fiber: owner.fiber,
                generation: owner.generation,
            });
        }
        let visible = matches!(data.state, FiberState::Active);
        let active = data.active.as_mut().ok_or(MetaError::StaleContext {
            fiber: owner.fiber,
            generation: owner.generation,
        })?;
        if active.services.contains_key(slot) || state.providers.contains_key(slot) {
            return Err(MetaError::DuplicateProvider {
                service: binding.key.clone(),
            });
        }
        let reservation =
            self.inner
                .resources
                .services
                .try_reserve(1)
                .ok_or(MetaError::CapacityExhausted {
                    resource: "services",
                })?;
        active.services.insert(
            slot.clone(),
            StagedService {
                binding: Arc::clone(binding),
                _reservation: reservation,
            },
        );
        state.providers.insert(
            slot.clone(),
            SupplyEntry {
                binding: Arc::clone(binding),
                visibility: if visible {
                    SupplyVisibility::Active
                } else {
                    SupplyVisibility::Loading
                },
            },
        );
        state.advance_revision();
        Ok(visible)
    }

    async fn withdraw_supply(
        &self,
        owner: Owner,
        slot: ServiceSlot,
        binding: Arc<ProviderBinding>,
    ) -> std::result::Result<(), String> {
        let (tickets, should_spawn) = {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            binding.lease.close();
            let fiber = state.fibers.get(&owner.fiber).cloned();
            if let Some(fiber) = fiber {
                let mut data = fiber.data.lock().expect("fiber state poisoned");
                if data.generation == owner.generation
                    && let Some(active) = data.active.as_mut()
                    && active
                        .services
                        .get(&slot)
                        .is_some_and(|current| Arc::ptr_eq(&current.binding, &binding))
                {
                    active.services.remove(&slot);
                }
            }
            let visibility = state
                .providers
                .get(&slot)
                .filter(|current| Arc::ptr_eq(&current.binding, &binding))
                .map(|current| current.visibility);
            if visibility.is_some() {
                state.providers.remove(&slot);
                state.advance_revision();
            }
            if visibility == Some(SupplyVisibility::Active) {
                Self::request_service_withdrawals_locked(
                    &mut state,
                    std::slice::from_ref(&slot),
                    Some(owner.fiber),
                )
            } else {
                (Vec::new(), None)
            }
        };
        self.start_reconciliation_requests(should_spawn);
        self.join_reconciliation_requests(tickets).await;
        binding.lease.seal();
        binding.lease.wait_drained().await;
        let endpoint = binding
            .endpoint
            .lock()
            .expect("service endpoint state poisoned")
            .take();
        if endpoint.is_some_and(drop_catching_unwind) {
            return Err("service endpoint destructor panicked".to_owned());
        }
        Ok(())
    }
}
