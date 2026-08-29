#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) struct LocalSlot {
    pub(super) contract: TypeId,
    pub(super) isolation: LocalIsolationId,
}

pub(crate) struct LocalBinding {
    pub(super) supply: LocalSupplyId,
    pub(super) key: LocalContractKey,
    pub(super) provider: FiberId,
    pub(super) generation: FiberGeneration,
    value: Arc<dyn Any + Send + Sync>,
}

impl LocalBinding {
    fn new<C: LocalContract>(
        supply: LocalSupplyId,
        provider: FiberId,
        generation: FiberGeneration,
        service: Arc<C::Service>,
    ) -> Self {
        Self {
            supply,
            key: LocalContractKey::new(C::KEY),
            provider,
            generation,
            value: Arc::new(service),
        }
    }

    pub(crate) fn service<C: LocalContract>(&self) -> Arc<C::Service> {
        Arc::clone(
            self.value
                .downcast_ref::<Arc<C::Service>>()
                .expect("a Local binding is retrieved only by its nominal marker TypeId"),
        )
    }
}

pub(super) struct LocalSupplyEntry {
    pub(super) binding: Arc<LocalBinding>,
    pub(super) visibility: SupplyVisibility,
}

/// Cloneable generation-owned handle for one exact Local service supply.
#[derive(Clone)]
pub struct LocalSupplyHandle {
    id: LocalSupplyId,
    key: LocalContractKey,
    effect: LocalSupplyEffect,
}

#[derive(Clone)]
enum LocalSupplyEffect {
    Setup {
        disposal: Arc<LocalSupplyDisposal>,
        effect: OwnedEffect,
    },
    Dynamic(EffectHandle),
}

struct LocalSupplyDisposal {
    runtime: Weak<RuntimeInner>,
    owner: Owner,
    slot: LocalSlot,
    binding: Arc<LocalBinding>,
    executor: tokio::runtime::Handle,
    maximum_diagnostic_entries: usize,
    maximum_diagnostic_bytes: usize,
    started: AtomicBool,
    result: Mutex<Option<std::result::Result<(), String>>>,
    complete: Notify,
}

impl LocalSupplyHandle {
    /// Returns the exact non-repeating Local supply identity.
    pub fn id(&self) -> LocalSupplyId {
        self.id
    }

    /// Returns the stable Host/Profile key of the supplied Local contract.
    pub fn key(&self) -> &LocalContractKey {
        &self.key
    }

    /// Withdraws this exact supply and joins hard-dependent convergence.
    ///
    /// Escaped `Arc`s remain ordinary Rust values and are deliberately not
    /// drained or revoked by this operation.
    pub async fn dispose(&self) -> CleanupReport {
        match &self.effect {
            LocalSupplyEffect::Setup { disposal, effect } => disposal.dispose(effect).await,
            LocalSupplyEffect::Dynamic(effect) => effect.dispose().await,
        }
    }
}

impl fmt::Debug for LocalSupplyHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalSupplyHandle")
            .field("id", &self.id)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl LocalSupplyDisposal {
    fn new(
        runtime: &Runtime,
        owner: Owner,
        slot: LocalSlot,
        binding: Arc<LocalBinding>,
        executor: tokio::runtime::Handle,
    ) -> Arc<Self> {
        let maximum_diagnostic_entries = runtime.inner.limits.payloads.maximum_diagnostic_entries;
        let maximum_diagnostic_bytes = runtime.inner.limits.payloads.maximum_diagnostic_bytes;
        Arc::new(Self {
            runtime: Arc::downgrade(&runtime.inner),
            owner,
            slot,
            binding,
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

    fn claim_start(&self) -> bool {
        self.started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn start_from_effect(self: &Arc<Self>) {
        if self.claim_start() {
            self.spawn(None);
        }
    }

    fn start_explicit(self: &Arc<Self>, effect: &OwnedEffect) {
        if self.claim_start() {
            self.spawn(effect.detach());
        }
    }

    fn spawn(self: &Arc<Self>, retention: Option<EffectRetention>) {
        let Some(inner) = self.runtime.upgrade() else {
            drop(retention);
            self.finish(Ok(()));
            return;
        };
        let runtime = Runtime { inner };
        let disposal = Arc::clone(self);
        self.executor.spawn(async move {
            let outcome = contain_panic_result(
                std::panic::AssertUnwindSafe(runtime.withdraw_local_supply(
                    disposal.owner,
                    disposal.slot,
                    Arc::clone(&disposal.binding),
                ))
                .catch_unwind()
                .await,
            );
            let result = outcome.unwrap_or_else(|_| {
                runtime.mark_terminal_owned("Local service supply cleanup driver panicked");
                Err("Local service supply cleanup driver panicked".to_owned())
            });
            drop(retention);
            disposal.finish(result);
        });
    }

    fn finish(&self, result: std::result::Result<(), String>) {
        let mut stored = self.result.lock().expect("Local supply disposal poisoned");
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
                .expect("Local supply disposal poisoned")
                .clone()
            {
                return result;
            }
            notified.as_mut().await;
        }
    }

    async fn dispose(self: &Arc<Self>, effect: &OwnedEffect) -> CleanupReport {
        self.start_explicit(effect);
        let mut report = CleanupReport::default();
        if let Err(error) = self.join().await {
            report.push_bounded(
                "withdraw Local service".to_owned(),
                error,
                self.maximum_diagnostic_entries,
                self.maximum_diagnostic_bytes,
            );
        }
        report
    }
}

impl Runtime {
    pub(super) fn provide_local<C: LocalContract>(
        &self,
        context: &Context,
        service: Arc<C::Service>,
    ) -> Result<LocalSupplyHandle> {
        let _runtime_admission = self.begin_admission(false)?;
        let owner = context.owner.ok_or_else(|| {
            MetaError::InvalidInput("the root context cannot provide a Local service".to_owned())
        })?;
        let token = self
            .inner
            .next_local_supply
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map_err(|_| MetaError::CapacityExhausted {
                resource: "Local supply identities",
            })?
            + 1;
        let id = LocalSupplyId::new(owner.fiber, owner.generation, token);
        let slot = LocalSlot {
            contract: TypeId::of::<C>(),
            isolation: context
                .local_isolation
                .get(&TypeId::of::<C>())
                .copied()
                .unwrap_or(LocalIsolationId(0)),
        };
        let binding = Arc::new(LocalBinding::new::<C>(
            id,
            owner.fiber,
            owner.generation,
            service,
        ));
        let fiber = self.owner_fiber(owner)?;
        let disposal = LocalSupplyDisposal::new(
            self,
            owner,
            slot,
            Arc::clone(&binding),
            fiber.executor.clone(),
        );
        let effect = if let Some(setup) = context
            .setup_effect
            .as_ref()
            .filter(|setup| setup.is_open())
        {
            let registration =
                setup.defer_owned("withdraw Local service".to_owned(), disposal.cleanup())?;
            if let Err(error) = self.register_local_supply(owner, slot, &binding) {
                drop(registration.detach());
                return Err(error);
            }
            LocalSupplyEffect::Setup {
                disposal,
                effect: registration,
            }
        } else {
            let mut transaction = self.begin_effect(owner, "dynamic Local service".to_owned())?;
            transaction.defer("withdraw Local service", disposal.cleanup())?;
            let visible = self.register_local_supply(owner, slot, &binding)?;
            if visible {
                self.notify_local_appearances(std::slice::from_ref(&slot), Some(owner.fiber));
            }
            LocalSupplyEffect::Dynamic(transaction.commit()?)
        };
        Ok(LocalSupplyHandle {
            id,
            key: LocalContractKey::new(C::KEY),
            effect,
        })
    }

    pub(super) fn lookup_local<C: LocalContract>(
        &self,
        context: &Context,
    ) -> Option<Arc<C::Service>> {
        let slot = LocalSlot {
            contract: TypeId::of::<C>(),
            isolation: context
                .local_isolation
                .get(&TypeId::of::<C>())
                .copied()
                .unwrap_or(LocalIsolationId(0)),
        };
        let state = self.inner.state.lock().expect("runtime state poisoned");
        if let Some(owner) = context.owner {
            let fiber = state.fibers.get(&owner.fiber)?;
            let data = fiber.data.lock().expect("fiber state poisoned");
            Runtime::validate_live_owner_data(owner, &data).ok()?;
        }
        let entry = state.local_providers.get(&slot)?;
        let visible = entry.visibility == SupplyVisibility::Active
            || context.owner.is_some_and(|owner| {
                owner.fiber == entry.binding.provider
                    && owner.generation == entry.binding.generation
            });
        visible.then(|| entry.binding.service::<C>())
    }

    fn register_local_supply(
        &self,
        owner: Owner,
        slot: LocalSlot,
        binding: &Arc<LocalBinding>,
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
        Runtime::validate_live_owner_data(owner, &data)?;
        let visible = matches!(data.state, FiberState::Active);
        let active = data.active.as_mut().ok_or(MetaError::StaleContext {
            fiber: owner.fiber,
            generation: owner.generation,
        })?;
        if active.local_services.contains_key(&slot) || state.local_providers.contains_key(&slot) {
            return Err(MetaError::DuplicateLocalProvider {
                contract: binding.key.clone(),
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
        active.local_services.insert(
            slot,
            StagedLocalService {
                binding: Arc::clone(binding),
                _reservation: reservation,
            },
        );
        state.local_providers.insert(
            slot,
            LocalSupplyEntry {
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

    async fn withdraw_local_supply(
        &self,
        owner: Owner,
        slot: LocalSlot,
        binding: Arc<LocalBinding>,
    ) -> std::result::Result<(), String> {
        let (tickets, should_spawn) = {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            if let Some(fiber) = state.fibers.get(&owner.fiber).cloned() {
                let mut data = fiber.data.lock().expect("fiber state poisoned");
                if data.generation == owner.generation
                    && let Some(active) = data.active.as_mut()
                    && active
                        .local_services
                        .get(&slot)
                        .is_some_and(|current| Arc::ptr_eq(&current.binding, &binding))
                {
                    active.local_services.remove(&slot);
                }
            }
            let visibility = state
                .local_providers
                .get(&slot)
                .filter(|current| Arc::ptr_eq(&current.binding, &binding))
                .map(|current| current.visibility);
            if visibility.is_some() {
                state.local_providers.remove(&slot);
                state.advance_revision();
            }
            if visibility == Some(SupplyVisibility::Active) {
                Self::request_local_withdrawals_locked(
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
        Ok(())
    }
}
