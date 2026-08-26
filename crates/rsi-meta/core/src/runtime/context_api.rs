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

    /// Consumes this value and associates one immutable safe-Rust extension.
    ///
    /// A new marker type consumes one Context entry. Replacing the value for
    /// an existing marker does not consume another entry. Cloned sibling
    /// Contexts continue to observe their original values.
    pub fn with_extension<K: ContextExtension>(mut self, value: K::Value) -> Result<Self> {
        let _runtime_admission = self.runtime.begin_admission(false)?;
        if !self.extensions.contains::<K>() {
            self.entries = self.next_context_entry_count()?;
        }
        Arc::make_mut(&mut self.extensions).insert::<K>(value);
        Ok(self)
    }

    /// Returns the immutable value associated with one extension marker.
    ///
    /// Reading an absent marker returns `None` without extending this Context
    /// or consuming Context-entry capacity.
    pub fn extension<K: ContextExtension>(&self) -> Option<Arc<K::Value>> {
        self.extensions.get::<K>()
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

    /// Consumes this value and appends one bounded direct-edge overlay layer.
    ///
    /// Only the selected layer list is copied when a cloned sibling shares it;
    /// the accumulated encoded length is maintained incrementally.
    pub fn intercept(mut self, service: impl AsRef<str>, layer: Value) -> Result<Self> {
        let layer = configuration::OwnedJsonValue::new(layer);
        let _runtime_admission = self.runtime.begin_admission(false)?;
        let service = service.as_ref();
        self.validate_context_key(service)?;
        let service = ServiceKey::new(service);
        let payloads = &self.runtime.inner.limits.payloads;
        let (layer, layer_bytes) = configuration::validate_owned_json_payload(
            layer,
            payloads,
            payloads.maximum_message_bytes,
        )?;
        let existing = self.intercepts.get(&service);
        let is_new_entry = existing.is_none();
        let previous_service_bytes = existing.map_or(2, |layers| layers.encoded_bytes);
        let separator_bytes = usize::from(existing.is_some_and(|layers| !layers.values.is_empty()));
        let encoded_bytes = previous_service_bytes
            .checked_add(separator_bytes)
            .and_then(|size| size.checked_add(layer_bytes))
            .ok_or(MetaError::PayloadTooLarge {
                maximum: payloads.maximum_message_bytes,
            })?;
        if encoded_bytes > payloads.maximum_message_bytes {
            return Err(MetaError::PayloadTooLarge {
                maximum: payloads.maximum_message_bytes,
            });
        }
        let key_bytes = if is_new_entry {
            configuration::encoded_json_size_bounded(
                service.as_str(),
                payloads.maximum_context_bytes,
            )
            .map_err(|_| MetaError::PayloadTooLarge {
                maximum: payloads.maximum_context_bytes,
            })?
        } else {
            0
        };
        let added_bytes = key_bytes
            .checked_add(if is_new_entry { 2 } else { 0 })
            .and_then(|size| size.checked_add(separator_bytes))
            .and_then(|size| size.checked_add(layer_bytes))
            .ok_or(MetaError::PayloadTooLarge {
                maximum: payloads.maximum_context_bytes,
            })?;
        let context_bytes = self.next_context_bytes(added_bytes)?;
        if is_new_entry {
            self.entries = self.next_context_entry_count()?;
        }
        let intercepts = Arc::make_mut(&mut self.intercepts);
        let layers = Arc::make_mut(
            intercepts
                .entry(service)
                .or_insert_with(|| Arc::new(InterceptLayers::empty())),
        );
        layers.values.push(layer.into_inner());
        layers.encoded_bytes = encoded_bytes;
        self.encoded_bytes = context_bytes;
        Ok(self)
    }

    fn validate_context_key(&self, service: &str) -> Result<()> {
        if service.len() > self.runtime.inner.limits.payloads.maximum_identifier_bytes {
            return Err(MetaError::InvalidInput(
                "context service identifier exceeds the configured byte limit".to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_event_key(&self, event: &str) -> Result<()> {
        if event.len() > self.runtime.inner.limits.payloads.maximum_identifier_bytes {
            return Err(MetaError::InvalidInput(
                "event identifier exceeds the configured byte limit".to_owned(),
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
        factory: Arc<dyn PluginFactory>,
        config: ConfigValue,
    ) -> impl std::future::Future<Output = Result<FiberHandle>> + '_ {
        let factory = RetainedFactory::new(factory);
        let config = configuration::OwnedJsonValue::new(config);
        async move {
            let deadline = tokio::time::Instant::now()
                .checked_add(self.runtime.inner.limits.deadlines.transition)
                .expect("validated transition deadline fits Tokio Instant");
            let maximum_diagnostic_bytes =
                self.runtime.inner.limits.payloads.maximum_diagnostic_bytes;
            let runtime = self.runtime.clone();
            let preparation = runtime.begin_plugin_preparation()?;
            let preparation = self.runtime.yield_reconciliation_slot(async move {
                tokio::task::spawn_blocking(move || {
                    runtime.prepare_admitted(factory, config, preparation)
                })
                .await
                .map_err(|error| {
                    MetaError::Activation(super::dispatch::bound_formatted_diagnostic(
                        format_args!("plugin preparation task failed: {error}"),
                        maximum_diagnostic_bytes,
                    ))
                })?
                .map_err(|error| {
                    super::dispatch::bound_error_diagnostic(error, maximum_diagnostic_bytes)
                })
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

    /// Immediately registers one effect-owned event listener.
    ///
    /// Loading listeners are visible because their exact removal effect is
    /// already installed. The returned handle can dispose only this listener.
    pub fn on(
        &self,
        event: impl AsRef<str>,
        handler: Arc<dyn EventHandler>,
        options: EventOptions,
    ) -> Result<EventHandle> {
        let event = event.as_ref();
        self.validate_event_key(event)?;
        let event = EventKey::new(event);
        self.runtime.add_listener(self, event, handler, options)
    }

    /// Dispatches a bounded event to the complete listener snapshot.
    ///
    /// No target is evaluated; every snapshotted listener remains eligible.
    /// This future must be polled inside a Tokio runtime.
    pub fn dispatch<'a>(
        &'a self,
        event: impl AsRef<str> + 'a,
        mode: DispatchMode,
        value: Value,
    ) -> impl std::future::Future<Output = Result<EventReceipt>> + 'a {
        let value = configuration::OwnedJsonValue::new(value);
        async move {
            let event = event.as_ref();
            self.validate_event_key(event)?;
            let event = EventKey::new(event);
            self.runtime
                .dispatch_event(self, &event, mode, value, None)
                .await
        }
    }

    /// Dispatches a bounded event through one generic listener target.
    ///
    /// The complete listener snapshot is selected before any callback
    /// admission or once claim. Selection runs on dispatch-bounded blocking
    /// work and is covered by the event deadline. Explicitly global listeners
    /// bypass `target`. This future must be polled inside a Tokio runtime.
    pub fn dispatch_targeted<'a>(
        &'a self,
        event: impl AsRef<str> + 'a,
        mode: DispatchMode,
        value: Value,
        target: Arc<dyn EventTarget>,
    ) -> impl std::future::Future<Output = Result<EventReceipt>> + 'a {
        let value = configuration::OwnedJsonValue::new(value);
        async move {
            let event = event.as_ref();
            self.validate_event_key(event)?;
            let event = EventKey::new(event);
            self.runtime
                .dispatch_event(self, &event, mode, value, Some(target))
                .await
        }
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
                    MetaError::InvalidConfig(super::dispatch::bound_formatted_diagnostic(
                        format_args!("validation task failed: {error}"),
                        maximum_diagnostic_bytes,
                    ))
                })?
                .map_err(|error| {
                    super::dispatch::bound_error_diagnostic(error, maximum_diagnostic_bytes)
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
                    MetaError::Activation(super::dispatch::bound_formatted_diagnostic(
                        format_args!("reconfiguration task failed: {error}"),
                        maximum_diagnostic_bytes,
                    ))
                })?
                .map_err(|error| {
                    super::dispatch::bound_error_diagnostic(error, maximum_diagnostic_bytes)
                })
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
            intercepts: Arc::clone(&self.base_context.intercepts),
            extensions: Arc::clone(&self.base_context.extensions),
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
            state: self.state.clone(),
        }
    }
}

pub(super) fn binding_identities(
    bindings: &BTreeMap<ServiceKey, Arc<ProviderBinding>>,
) -> BTreeMap<ServiceKey, SupplyId> {
    bindings
        .iter()
        .map(|(key, binding)| (key.clone(), binding.supply))
        .collect()
}
