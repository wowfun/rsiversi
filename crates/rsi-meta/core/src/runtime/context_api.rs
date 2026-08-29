#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;

impl Context {
    pub(crate) fn ensure_same_authority(&self, other: &Self) -> Result<()> {
        if !Arc::ptr_eq(&self.runtime.inner, &other.runtime.inner) {
            return Err(MetaError::CapabilityFromDifferentRuntime);
        }
        if self.owner != other.owner {
            return Err(MetaError::StaleCapability);
        }
        Ok(())
    }

    /// Returns the retained Runtime that owns this scope.
    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// Returns the owning Fiber generation, or `None` for a root Context.
    pub fn owner(&self) -> Option<(FiberId, FiberGeneration)> {
        self.owner.map(|owner| (owner.fiber, owner.generation))
    }

    /// Returns the currently visible safe-Rust Local object, if any.
    ///
    /// This point-in-time lookup does not create a managed dependency edge.
    /// An escaped `Arc` continues to name that ordinary Rust value after its
    /// supply is withdrawn.
    pub fn lookup_local<C: LocalContract>(&self) -> Option<Arc<C::Service>> {
        self.runtime.lookup_local::<C>(self)
    }

    /// Supplies one generation-owned safe-Rust Local object directly.
    pub fn provide_local<C: LocalContract>(
        &self,
        service: Arc<C::Service>,
    ) -> Result<LocalSupplyHandle> {
        if C::KEY.len() > self.runtime.inner.limits.payloads.maximum_identifier_bytes {
            return Err(MetaError::InvalidInput(
                "Local contract identifier exceeds the configured byte limit".to_owned(),
            ));
        }
        self.runtime.provide_local::<C>(self, service)
    }

    /// Consumes this value and selects an explicit isolation for one Local contract.
    pub fn isolate_local<C: LocalContract>(self, isolation: LocalIsolationId) -> Result<Self> {
        if C::KEY.len() > self.runtime.inner.limits.payloads.maximum_identifier_bytes {
            return Err(MetaError::InvalidInput(
                "Local contract identifier exceeds the configured byte limit".to_owned(),
            ));
        }
        self.isolate_local_type(TypeId::of::<C>(), C::KEY, isolation)
    }

    /// Selects an explicit isolation for one resolver-validated Local type.
    ///
    /// Generic Hosts use this after mapping `catalog_key` to its frozen nominal
    /// [`TypeId`]. The Runtime derives accounting from that exact key. Product plugins should prefer
    /// [`Self::isolate_local`].
    pub fn isolate_local_type(
        mut self,
        contract: TypeId,
        catalog_key: &str,
        isolation: LocalIsolationId,
    ) -> Result<Self> {
        let _runtime_admission = self.runtime.begin_admission(false)?;
        if catalog_key.len() > self.runtime.inner.limits.payloads.maximum_identifier_bytes {
            return Err(MetaError::InvalidInput(
                "Local contract identifier exceeds the configured byte limit".to_owned(),
            ));
        }
        if !self.local_isolation.contains_key(&contract) {
            self.entries = self.next_context_entry_count()?;
            self.encoded_bytes = self.next_context_bytes(
                catalog_key
                    .len()
                    .checked_add(std::mem::size_of::<LocalIsolationId>())
                    .ok_or(MetaError::PayloadTooLarge {
                        maximum: self.runtime.inner.limits.payloads.maximum_context_bytes,
                    })?,
            )?;
        }
        Arc::make_mut(&mut self.local_isolation).insert(contract, isolation);
        Ok(self)
    }

    /// Consumes this value and selects a newly allocated Local isolation identity.
    pub fn isolate_local_fresh<C: LocalContract>(self) -> Result<(Self, LocalIsolationId)> {
        let isolation = self.runtime.next_local_isolation_id()?;
        Ok((self.isolate_local::<C>(isolation)?, isolation))
    }

    /// Allocates and selects an isolation for one resolver-validated Local type.
    pub fn isolate_local_type_fresh(
        self,
        contract: TypeId,
        catalog_key: &str,
    ) -> Result<(Self, LocalIsolationId)> {
        let isolation = self.runtime.next_local_isolation_id()?;
        Ok((
            self.isolate_local_type(contract, catalog_key, isolation)?,
            isolation,
        ))
    }

    /// Consumes this value and selects an explicit isolation for one typed Local event.
    pub fn isolate_event<E: LocalEvent>(self, isolation: LocalIsolationId) -> Result<Self> {
        if E::KEY.len() > self.runtime.inner.limits.payloads.maximum_identifier_bytes {
            return Err(MetaError::InvalidInput(
                "Local event identifier exceeds the configured byte limit".to_owned(),
            ));
        }
        self.isolate_event_type(TypeId::of::<E>(), E::KEY, isolation)
    }

    /// Selects an explicit isolation for one resolver-validated Local event type.
    /// The Runtime derives accounting from the exact resolver-owned `catalog_key`.
    pub fn isolate_event_type(
        mut self,
        event: TypeId,
        catalog_key: &str,
        isolation: LocalIsolationId,
    ) -> Result<Self> {
        let _runtime_admission = self.runtime.begin_admission(false)?;
        if catalog_key.len() > self.runtime.inner.limits.payloads.maximum_identifier_bytes {
            return Err(MetaError::InvalidInput(
                "Local event identifier exceeds the configured byte limit".to_owned(),
            ));
        }
        if !self.event_isolation.contains_key(&event) {
            self.entries = self.next_context_entry_count()?;
            self.encoded_bytes = self.next_context_bytes(
                catalog_key
                    .len()
                    .checked_add(std::mem::size_of::<LocalIsolationId>())
                    .ok_or(MetaError::PayloadTooLarge {
                        maximum: self.runtime.inner.limits.payloads.maximum_context_bytes,
                    })?,
            )?;
        }
        Arc::make_mut(&mut self.event_isolation).insert(event, isolation);
        Ok(self)
    }

    /// Consumes this value and selects a newly allocated typed-event isolation.
    pub fn isolate_event_fresh<E: LocalEvent>(self) -> Result<(Self, LocalIsolationId)> {
        let isolation = self.runtime.next_local_isolation_id()?;
        Ok((self.isolate_event::<E>(isolation)?, isolation))
    }

    /// Allocates and selects an isolation for one resolver-validated event type.
    pub fn isolate_event_type_fresh(
        self,
        event: TypeId,
        catalog_key: &str,
    ) -> Result<(Self, LocalIsolationId)> {
        let isolation = self.runtime.next_local_isolation_id()?;
        Ok((
            self.isolate_event_type(event, catalog_key, isolation)?,
            isolation,
        ))
    }

    /// Registers one effect-owned synchronous [`crate::Emit`] listener.
    pub fn on_emit<E, H>(
        &self,
        handler: Arc<H>,
        options: LocalEventOptions,
    ) -> Result<LocalEventHandle>
    where
        E: LocalEvent<Mode = crate::Emit>,
        H: crate::EmitEventHandler<E>,
    {
        self.validate_local_event_key::<E>()?;
        let handler: Arc<dyn crate::EmitEventHandler<E>> = handler;
        self.runtime
            .add_local_listener::<E, _>(self, handler, options)
    }

    /// Registers one effect-owned asynchronous [`crate::Parallel`] listener.
    pub fn on_parallel<E, H>(
        &self,
        handler: Arc<H>,
        options: LocalEventOptions,
    ) -> Result<LocalEventHandle>
    where
        E: LocalEvent<Mode = crate::Parallel>,
        H: crate::ParallelEventHandler<E>,
    {
        self.validate_local_event_key::<E>()?;
        let handler: Arc<dyn crate::ParallelEventHandler<E>> = handler;
        self.runtime
            .add_local_listener::<E, _>(self, handler, options)
    }

    /// Registers one effect-owned asynchronous [`crate::Serial`] listener.
    pub fn on_serial<E, H>(
        &self,
        handler: Arc<H>,
        options: LocalEventOptions,
    ) -> Result<LocalEventHandle>
    where
        E: LocalEvent<Mode = crate::Serial>,
        H: crate::SerialEventHandler<E>,
    {
        self.validate_local_event_key::<E>()?;
        let handler: Arc<dyn crate::SerialEventHandler<E>> = handler;
        self.runtime
            .add_local_listener::<E, _>(self, handler, options)
    }

    /// Registers one effect-owned synchronous [`crate::Bail`] listener.
    pub fn on_bail<E, H>(
        &self,
        handler: Arc<H>,
        options: LocalEventOptions,
    ) -> Result<LocalEventHandle>
    where
        E: LocalEvent<Mode = crate::Bail>,
        H: crate::BailEventHandler<E>,
    {
        self.validate_local_event_key::<E>()?;
        let handler: Arc<dyn crate::BailEventHandler<E>> = handler;
        self.runtime
            .add_local_listener::<E, _>(self, handler, options)
    }

    /// Registers one effect-owned synchronous [`crate::Waterfall`] middleware.
    pub fn on_waterfall<E, H>(
        &self,
        handler: Arc<H>,
        options: LocalEventOptions,
    ) -> Result<LocalEventHandle>
    where
        E: LocalEvent<Mode = crate::Waterfall>,
        H: crate::WaterfallEventHandler<E>,
    {
        self.validate_local_event_key::<E>()?;
        let handler: Arc<dyn crate::WaterfallEventHandler<E>> = handler;
        self.runtime
            .add_local_listener::<E, _>(self, handler, options)
    }

    /// Dispatches one typed Local event using the mode fixed by its marker.
    pub fn dispatch_local<E: LocalEvent>(
        &self,
        value: E::Value,
    ) -> Result<<E::Mode as LocalEventMode<E>>::Dispatch> {
        let snapshot = self.runtime.snapshot_local_event::<E>(self)?;
        Ok(E::Mode::dispatch(snapshot, value))
    }

    fn validate_local_event_key<E: LocalEvent>(&self) -> Result<()> {
        if E::KEY.len() > self.runtime.inner.limits.payloads.maximum_identifier_bytes {
            return Err(MetaError::InvalidInput(
                "Local event identifier exceeds the configured byte limit".to_owned(),
            ));
        }
        Ok(())
    }

    /// Consumes this value and selects an explicit isolation for one service.
    ///
    /// Unbranched scope chains mutate their uniquely owned map in place; a
    /// cloned sibling causes copy-on-write and remains unchanged.
    pub fn isolate(self, service: impl AsRef<str>, isolation: IsolationId) -> Result<Self> {
        let _runtime_admission = self.runtime.begin_admission(false)?;
        self.isolate_admitted(service.as_ref(), isolation)
    }

    fn isolate_admitted(mut self, service: &str, isolation: IsolationId) -> Result<Self> {
        self.validate_context_key(service)?;
        let service = ServiceKey::new(service);
        if !self.isolation.contains_key(&service) {
            self.entries = self.next_context_entry_count()?;
            self.encoded_bytes = self.next_context_bytes(
                service
                    .as_str()
                    .len()
                    .checked_add(std::mem::size_of::<IsolationId>())
                    .ok_or(MetaError::PayloadTooLarge {
                        maximum: self.runtime.inner.limits.payloads.maximum_context_bytes,
                    })?,
            )?;
        }
        Arc::make_mut(&mut self.isolation).insert(service, isolation);
        Ok(self)
    }

    /// Consumes this value and selects a newly allocated isolation identity.
    pub fn isolate_fresh(self, service: impl AsRef<str>) -> Result<(Self, IsolationId)> {
        let _runtime_admission = self.runtime.begin_admission(false)?;
        let isolation = self.runtime.next_isolation_id()?;
        Ok((
            self.isolate_admitted(service.as_ref(), isolation)?,
            isolation,
        ))
    }

    fn validate_context_key(&self, service: &str) -> Result<()> {
        if service.len() > self.runtime.inner.limits.payloads.maximum_identifier_bytes {
            return Err(MetaError::InvalidInput(
                "context service identifier exceeds the configured byte limit".to_owned(),
            ));
        }
        Ok(())
    }

    fn next_context_entry_count(&self) -> Result<usize> {
        let entries = self
            .entries
            .checked_add(1)
            .ok_or(MetaError::CapacityExhausted {
                resource: "context entries",
            })?;
        if entries > self.runtime.inner.limits.topology.maximum_context_entries {
            return Err(MetaError::CapacityExhausted {
                resource: "context entries",
            });
        }
        Ok(entries)
    }

    fn next_context_bytes(&self, added: usize) -> Result<usize> {
        let maximum = self.runtime.inner.limits.payloads.maximum_context_bytes;
        let bytes = self
            .encoded_bytes
            .checked_add(added)
            .ok_or(MetaError::PayloadTooLarge { maximum })?;
        if bytes > maximum {
            return Err(MetaError::PayloadTooLarge { maximum });
        }
        Ok(bytes)
    }

    /// Prepares and applies a factory as a child of this Context.
    ///
    /// This future must be polled inside a Tokio runtime. One absolute
    /// transition deadline includes preparation and convergence. Timeout or
    /// caller cancellation cannot stop blocking work or Runtime-owned rollback.
    pub fn apply(
        &self,
        factory: ResolvedFactory,
        config: ConfigValue,
    ) -> impl std::future::Future<Output = Result<FiberHandle>> + '_ {
        let config = configuration::OwnedJsonValue::new(config);
        async move {
            let maximum_diagnostic_bytes =
                self.runtime.inner.limits.payloads.maximum_diagnostic_bytes;
            let runtime = self.runtime.clone();
            let preparation = runtime.begin_plugin_preparation()?;
            let (identity, update_mode, implementation) = factory.into_parts();
            let factory = RetainedFactory::new(implementation);
            let deadline = tokio::time::Instant::now()
                .checked_add(self.runtime.inner.limits.deadlines.transition)
                .expect("validated transition deadline fits Tokio Instant");
            let preparation = self.runtime.yield_reconciliation_slot(async move {
                tokio::task::spawn_blocking(move || {
                    runtime.prepare_admitted(identity, update_mode, factory, config, preparation)
                })
                .await
                .map_err(|error| {
                    MetaError::Activation(super::diagnostics::bound_formatted(
                        format_args!("plugin preparation task failed: {error}"),
                        maximum_diagnostic_bytes,
                    ))
                })?
                .map_err(|error| super::diagnostics::bound_error(error, maximum_diagnostic_bytes))
            });
            let prepared = tokio::time::timeout_at(deadline, preparation)
                .await
                .map_err(|_| MetaError::Timeout("plugin transition"))??;
            tokio::time::timeout_at(deadline, self.runtime.apply_prepared(self, prepared))
                .await
                .map_err(|_| MetaError::Timeout("plugin transition"))?
        }
    }

    /// Applies an already validated and normalized preparation proof.
    ///
    /// This future must be polled inside a Tokio runtime. Its absolute
    /// transition deadline drops only the waiter; an inserted unacknowledged
    /// Fiber remains Runtime-owned through disposal.
    pub async fn apply_prepared(&self, prepared: PreparedPlugin) -> Result<FiberHandle> {
        let deadline = tokio::time::Instant::now()
            .checked_add(self.runtime.inner.limits.deadlines.transition)
            .expect("validated transition deadline fits Tokio Instant");
        tokio::time::timeout_at(deadline, self.runtime.apply_prepared(self, prepared))
            .await
            .map_err(|_| MetaError::Timeout("plugin transition"))?
    }

    /// Begins one wrapper-first effect transaction on the owning generation.
    ///
    /// The wrapper is owned by the generation before this method returns.
    /// Fiber retirement waits for an open transaction to commit, abort, or be
    /// dropped before it runs the transaction's reverse-ordered cleanups.
    pub fn begin_effect(&self, label: impl Into<String>) -> Result<EffectTxn> {
        let owner = self.owner.ok_or_else(|| {
            MetaError::InvalidInput("the root context cannot own an effect".to_owned())
        })?;
        if let Some(setup) = &self.setup_effect {
            if setup.is_open() {
                return Err(MetaError::InvalidInput(
                    "activation setup already owns the open effect transaction".to_owned(),
                ));
            }
            self.runtime.ensure_dynamic_effect_owner(owner)?;
        }
        let label = label.into();
        self.runtime.validate_effect_label(&label)?;
        self.runtime.begin_effect(owner, label)
    }

    /// Registers one reverse-ordered cleanup effect on the owning generation.
    ///
    /// This shorthand opens a transaction, installs one cleanup, and commits
    /// it. Use [`Self::begin_effect`] when setup can fail or yield.
    pub fn defer(&self, label: impl Into<String>, cleanup: Cleanup) -> Result<()> {
        let label = label.into();
        if let Some(setup) = &self.setup_effect
            && setup.is_open()
        {
            return setup.defer(label, cleanup);
        }
        let mut transaction = self.begin_effect(label.clone())?;
        transaction.defer(label, cleanup)?;
        let _handle = transaction.commit()?;
        Ok(())
    }

    /// Dynamically supplies one endpoint from the owning generation.
    ///
    /// A Loading supply immediately occupies its isolated slot and is visible
    /// to its own Context, but external injection observes it only after the
    /// provider generation becomes Active. The returned handle can withdraw
    /// only this exact non-repeating supply.
    pub fn provide(
        &self,
        key: impl AsRef<str>,
        contract: impl AsRef<str>,
        version: ContractVersion,
        endpoint: Arc<dyn ServiceEndpoint>,
    ) -> Result<SupplyHandle> {
        let key = key.as_ref();
        if key.len() > self.runtime.inner.limits.payloads.maximum_identifier_bytes {
            return Err(MetaError::InvalidInput(
                "service identifier exceeds the configured byte limit".to_owned(),
            ));
        }
        let contract = contract.as_ref();
        if contract.len() > self.runtime.inner.limits.payloads.maximum_identifier_bytes {
            return Err(MetaError::InvalidInput(
                "service contract identifier exceeds the configured byte limit".to_owned(),
            ));
        }
        self.runtime.provide(
            self,
            ServiceKey::new(key),
            ContractId::new(contract),
            version,
            endpoint,
        )
    }

    /// Dynamically supplies one endpoint and captures its self-visible authority.
    ///
    /// Capability admission completes before the supply can occupy its slot. If
    /// capture fails, no supply is registered and no withdrawal handle is
    /// required.
    pub fn provide_and_capture(
        &self,
        key: impl AsRef<str>,
        contract: impl AsRef<str>,
        version: ContractVersion,
        endpoint: Arc<dyn ServiceEndpoint>,
    ) -> Result<(SupplyHandle, Capability)> {
        let key = key.as_ref();
        if key.len() > self.runtime.inner.limits.payloads.maximum_identifier_bytes {
            return Err(MetaError::InvalidInput(
                "service identifier exceeds the configured byte limit".to_owned(),
            ));
        }
        let contract = contract.as_ref();
        if contract.len() > self.runtime.inner.limits.payloads.maximum_identifier_bytes {
            return Err(MetaError::InvalidInput(
                "service contract identifier exceeds the configured byte limit".to_owned(),
            ));
        }
        self.runtime.provide_and_capture(
            self,
            ServiceKey::new(key),
            ContractId::new(contract),
            version,
            endpoint,
        )
    }

    /// Captures the exact service binding resolved for the owning generation.
    pub fn service(&self, key: impl AsRef<str>) -> Result<Capability> {
        let key = key.as_ref();
        if key.len() > self.runtime.inner.limits.payloads.maximum_identifier_bytes {
            return Err(MetaError::InvalidInput(
                "service identifier exceeds the configured byte limit".to_owned(),
            ));
        }
        self.runtime.service(self, &ServiceKey::new(key))
    }
}

impl FiberHandle {
    /// Returns the stable Runtime-local Fiber identity.
    pub fn id(&self) -> FiberId {
        self.fiber.id
    }

    /// Returns the Fiber's current observable state.
    pub fn snapshot(&self) -> FiberSnapshot {
        self.fiber.snapshot()
    }

    /// Subscribes to later Fiber snapshots.
    pub fn subscribe(&self) -> watch::Receiver<FiberSnapshot> {
        self.fiber.watch.subscribe()
    }

    /// Waits until the Fiber is not loading or unloading.
    ///
    /// This future must be polled inside a Tokio runtime.
    pub async fn wait_settled(&self) -> FiberSnapshot {
        let mut receiver = self.subscribe();
        loop {
            let snapshot = receiver.borrow().clone();
            if !snapshot.state.is_transitioning() {
                return snapshot;
            }
            if receiver.changed().await.is_err() {
                return self.snapshot();
            }
        }
    }

    /// Waits for an active generation, terminal Fiber state, or cancellation.
    ///
    /// This future must be polled inside a Tokio runtime.
    pub async fn wait_active(&self, cancellation: &CancellationToken) -> Result<FiberSnapshot> {
        let mut receiver = self.subscribe();
        loop {
            let snapshot = receiver.borrow().clone();
            match &snapshot.state {
                FiberState::Active => return Ok(snapshot),
                FiberState::Failed(error) => {
                    return Err(MetaError::Activation(error.clone()));
                }
                FiberState::Disposed => {
                    return Err(MetaError::FiberDisposed { fiber: self.id() });
                }
                FiberState::Pending(_) | FiberState::Loading | FiberState::Unloading => {}
            }
            tokio::select! {
                () = cancellation.cancelled() => return Err(MetaError::Cancelled),
                changed = receiver.changed() => {
                    if changed.is_err() {
                        return Err(MetaError::FiberDisposed { fiber: self.id() });
                    }
                }
            }
        }
    }

    /// Replaces retained configuration and converges one serialized generation.
    ///
    /// This future must be polled inside a Tokio runtime. One absolute
    /// transition deadline includes normalization and convergence. Once
    /// admitted, timeout or caller cancellation detaches only the waiter.
    #[allow(clippy::too_many_lines)] // The returned future owns serialized preparation, installation, and convergence.
    pub fn reconfigure(
        &self,
        config: ConfigValue,
    ) -> impl std::future::Future<Output = Result<FiberSnapshot>> + '_ {
        let config = configuration::OwnedJsonValue::new(config);
        async move {
            let deadline = tokio::time::Instant::now()
                .checked_add(self.runtime.inner.limits.deadlines.transition)
                .expect("validated transition deadline fits Tokio Instant");
            let maximum_diagnostic_bytes =
                self.runtime.inner.limits.payloads.maximum_diagnostic_bytes;
            let runtime = self.runtime.clone();
            let fiber = Arc::clone(&self.fiber);
            let configuration =
                Arc::clone(&fiber.configuration)
                    .try_lock_owned()
                    .map_err(|_| MetaError::Busy {
                        operation: "plugin reconfiguration",
                    })?;
            let (preparation, attempt_reservations) = runtime.begin_attempt_preparation()?;
            let operation = tokio::spawn(async move {
                let _configuration = configuration;
                let (factory, desired_revision) = {
                    let data = fiber.data.lock().expect("fiber state poisoned");
                    if data.disposed {
                        return Err(MetaError::FiberDisposed { fiber: fiber.id });
                    }
                    let revision = data
                        .desired
                        .as_ref()
                        .expect("registered Fiber retains its desired configuration")
                        .revision
                        .checked_add(1)
                        .ok_or(MetaError::CapacityExhausted {
                            resource: "desired configuration revisions",
                        })?;
                    (
                        data.factory
                            .as_ref()
                            .expect("registered Fiber retains its factory")
                            .clone(),
                        revision,
                    )
                };
                let preparing_runtime = runtime.clone();
                let preparing_factory = factory.clone();
                let (desired, attempt) = tokio::task::spawn_blocking(move || {
                    preparing_runtime.prepare_attempt_admitted(
                        &preparing_factory,
                        config,
                        desired_revision,
                        preparation,
                        attempt_reservations,
                    )
                })
                .await
                .map_err(|error| {
                    MetaError::InvalidConfig(super::diagnostics::bound_formatted(
                        format_args!("validation task failed: {error}"),
                        maximum_diagnostic_bytes,
                    ))
                })?
                .map_err(|error| {
                    super::diagnostics::bound_error(error, maximum_diagnostic_bytes)
                })?;
                let (retired_desired, retired_attempts) = {
                    let mut state = runtime.inner.state.lock().expect("runtime state poisoned");
                    let mut data = fiber.data.lock().expect("fiber state poisoned");
                    if data.disposed {
                        return Err(MetaError::FiberDisposed { fiber: fiber.id });
                    }
                    data.target_revision = desired.revision;
                    let retired_desired = data.desired.replace(desired);
                    let mut retired_attempts = Vec::new();
                    if data.active.is_some() {
                        if let Some(previous) = data.replacement.replace(attempt) {
                            retired_attempts.push(previous);
                        }
                    } else {
                        let previous = data
                            .attempt
                            .replace(attempt)
                            .expect("registered Fiber retains its prepared attempt");
                        Runtime::replace_dependent_requirements(
                            &mut state,
                            &fiber,
                            &previous,
                            data.attempt
                                .as_ref()
                                .expect("replacement attempt was installed"),
                        );
                        retired_attempts.push(previous);
                        if let Some(replacement) = data.replacement.take() {
                            retired_attempts.push(replacement);
                        }
                    }
                    if matches!(data.state, FiberState::Failed(_)) && data.active.is_none() {
                        data.state = FiberState::Pending(PendingReport::default());
                    }
                    data.last_attempt = None;
                    (retired_desired, retired_attempts)
                };
                drop(retired_desired);
                drop(retired_attempts);
                let snapshot = if let Some(ticket) = runtime.request_reconciliation(fiber.id) {
                    ticket.join().await
                } else {
                    fiber.snapshot()
                };
                Ok(snapshot)
            });
            tokio::time::timeout_at(deadline, operation)
                .await
                .map_err(|_| MetaError::Timeout("plugin transition"))?
                .map_err(|error| {
                    MetaError::Activation(super::diagnostics::bound_formatted(
                        format_args!("reconfiguration task failed: {error}"),
                        maximum_diagnostic_bytes,
                    ))
                })?
                .map_err(|error| super::diagnostics::bound_error(error, maximum_diagnostic_bytes))
        }
    }

    /// Joins idempotent child/effect teardown and returns every cleanup failure.
    ///
    /// This future must be polled inside a Tokio runtime. Once initiated,
    /// disposal remains Runtime-owned if this future is dropped.
    pub async fn dispose(&self) -> CleanupReport {
        self.runtime
            .dispose_fiber_instance(Arc::clone(&self.fiber))
            .await
    }
}

impl fmt::Debug for FiberHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FiberHandle")
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

impl Fiber {
    pub(super) fn context(&self, generation: FiberGeneration) -> Context {
        Context {
            runtime: Runtime {
                inner: self.runtime.upgrade().expect("runtime outlives fibers"),
            },
            owner: Some(Owner {
                fiber: self.id,
                generation,
            }),
            setup_effect: None,
            isolation: Arc::clone(&self.base_context.isolation),
            local_isolation: Arc::clone(&self.base_context.local_isolation),
            event_isolation: Arc::clone(&self.base_context.event_isolation),
            entries: self.base_context.entries,
            encoded_bytes: self.base_context.encoded_bytes,
            trace: self.base_context.trace.clone(),
        }
    }

    pub(super) fn snapshot(&self) -> FiberSnapshot {
        self.data
            .lock()
            .expect("fiber state poisoned")
            .snapshot(self.id)
    }

    pub(super) fn set_state(&self, state: FiberState) {
        let mut data = self.data.lock().expect("fiber state poisoned");
        data.state = state;
        let snapshot = data.snapshot(self.id);
        self.watch.send_replace(snapshot);
    }
}

impl FiberData {
    pub(super) fn snapshot(&self, id: FiberId) -> FiberSnapshot {
        FiberSnapshot {
            id,
            generation: self.generation,
            factory: self.identity.clone(),
            update_mode: self.update_mode,
            state: self.state.clone(),
        }
    }
}

pub(super) fn binding_identities(
    bindings: &BTreeMap<ServiceKey, Arc<ProviderBinding>>,
    local_bindings: &BTreeMap<TypeId, Arc<LocalBinding>>,
    fiber: &Fiber,
) -> BindingIdentities {
    BindingIdentities {
        portable: bindings
            .iter()
            .map(|(key, binding)| (key.clone(), binding.supply))
            .collect(),
        local: local_bindings
            .iter()
            .map(|(contract, binding)| (fiber.base_context.local_slot(*contract), binding.supply))
            .collect(),
    }
}
